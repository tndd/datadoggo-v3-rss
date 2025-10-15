use std::collections::BTreeMap;
use std::fs;
use std::time::Duration;

use anyhow::Result;
use feed_rs::{model::Entry, parser};
use futures::{stream, StreamExt};
use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::Client;
use sqlx::PgPool;
use tracing::{error, info, warn};
use uuid::Uuid;

use serde::{Deserialize, Serialize};

use crate::models::{NewQueue, RssFeedSource, RssLinks};
use crate::webhook;

static URL_PATTERN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"https?://[^\s\"'<>()]+"#).expect("URL正規表現のコンパイルに失敗"));

/// RSS取得リクエストのタイムアウト秒数（スクレイピング側と揃えている）
const FETCH_RSS_TIMEOUT_SECS: u64 = 15;
/// RSS取得時に同時実行する最大フィード数
const MAX_CONCURRENT_FEED_REQUESTS: usize = 8;

#[derive(Debug, Serialize, Deserialize)]
pub struct FetchRssFeedResult {
    pub group: String,
    pub name: String,
    pub processed: usize,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FetchRssSummary {
    pub total_processed: usize,
    pub feeds: Vec<FetchRssFeedResult>,
}

/// rss_links.ymlを読み込む
pub fn load_rss_links(path: &str) -> Result<Vec<RssFeedSource>> {
    let content = fs::read_to_string(path)?;
    let links: RssLinks = serde_yaml::from_str(&content)?;
    Ok(links.into_sources())
}

/// RSSフィードを取得してパース
pub async fn fetch_and_parse_feed(
    client: &Client,
    url: &str,
    group: Option<&str>,
) -> Result<Vec<NewQueue>> {
    let response = client.get(url).send().await?;
    let content = response.bytes().await?;
    parse_feed_content(&content, group)
}

pub(crate) fn parse_feed_content(content: &[u8], group: Option<&str>) -> Result<Vec<NewQueue>> {
    let feed = parser::parse(content)?;

    let mut entries = Vec::new();

    for entry in feed.entries {
        let Some(link) = extract_link(&entry) else {
            continue;
        };

        let title = entry
            .title
            .map(|t| t.content)
            .unwrap_or_else(|| "No title".to_string());

        let pub_date = entry.published.or(entry.updated);

        let description = entry
            .summary
            .map(|t| t.content)
            .or_else(|| entry.content.and_then(|c| c.body))
            .unwrap_or_else(String::new);

        entries.push(NewQueue {
            link,
            title,
            pub_date,
            description,
            group: group.map(|g| g.to_string()),
        });
    }

    Ok(entries)
}

pub(crate) fn extract_link(entry: &Entry) -> Option<String> {
    entry
        .links
        .iter()
        .map(|link| link.href.trim())
        .find(|href| !href.is_empty())
        .map(|href| href.to_string())
        .or_else(|| {
            entry
                .summary
                .as_ref()
                .and_then(|s| find_first_url(&s.content))
        })
        .or_else(|| {
            entry
                .content
                .as_ref()
                .and_then(|c| c.body.as_ref())
                .and_then(|body| find_first_url(body))
        })
}

pub(crate) fn find_first_url(text: &str) -> Option<String> {
    let matched = URL_PATTERN.find(text)?;
    let trimmed = matched
        .as_str()
        .trim_end_matches(|c: char| matches!(c, ')' | ']' | '"' | '\'' | ',' | '.' | ';'))
        .to_string();

    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// queueテーブルにupsert（INSERT or UPDATE）
pub async fn upsert_queue_entries(
    pool: &PgPool,
    entries: Vec<NewQueue>,
    group: Option<String>,
) -> Result<usize> {
    let mut count = 0;

    for entry in entries {
        let group_value = group.clone().or(entry.group.clone());
        let id = Uuid::new_v4();

        sqlx::query(
            r#"
            INSERT INTO rss.queue (id, link, title, pub_date, description, "group")
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (link)
            DO UPDATE SET
                title = EXCLUDED.title,
                pub_date = EXCLUDED.pub_date,
                description = EXCLUDED.description,
                "group" = EXCLUDED."group",
                updated_at = NOW()
            "#,
        )
        .bind(id)
        .bind(&entry.link)
        .bind(&entry.title)
        .bind(entry.pub_date)
        .bind(&entry.description)
        .bind(&group_value)
        .execute(pool)
        .await?;

        count += 1;
    }

    Ok(count)
}

/// fetch-rssコマンドのメイン処理
pub async fn run(pool: PgPool, webhook_url: Option<&str>) -> Result<()> {
    info!("rss_links.ymlを読み込み中...");
    let summary = execute_fetch_rss(&pool, "rss_links.yml").await?;

    if summary.feeds.is_empty() {
        info!("登録されているRSSフィードがありません");
        return Ok(());
    }

    log_fetch_rss_summary(&summary);

    if let Err(e) = webhook::notify_fetch_rss(webhook_url, &summary, "cli").await {
        warn!(error = %e, "Webhook送信に失敗しました(fetch-rss)");
    }

    Ok(())
}

pub(crate) fn log_fetch_rss_summary(summary: &FetchRssSummary) {
    let mut grouped: BTreeMap<&str, Vec<&FetchRssFeedResult>> = BTreeMap::new();
    for feed in &summary.feeds {
        grouped.entry(&feed.group).or_default().push(feed);
    }

    for (group_name, feeds) in grouped {
        info!(group = group_name, "グループの処理結果");
        for feed in feeds {
            match &feed.error {
                Some(err) => {
                    error!(group = %feed.group, name = %feed.name, %err, "RSS処理に失敗");
                }
                None => {
                    info!(
                        group = %feed.group,
                        name = %feed.name,
                        processed = feed.processed,
                        "RSSを処理"
                    );
                }
            }
        }
    }

    info!(total_processed = summary.total_processed, "RSS処理が完了");
}

/// fetch-rssのメインロジックを実行し、結果を返す
pub async fn execute_fetch_rss(pool: &PgPool, rss_links_path: &str) -> Result<FetchRssSummary> {
    let feeds = load_rss_links(rss_links_path)?;

    if feeds.is_empty() {
        return Ok(FetchRssSummary {
            total_processed: 0,
            feeds: Vec::new(),
        });
    }

    let client = Client::builder()
        .timeout(Duration::from_secs(FETCH_RSS_TIMEOUT_SECS))
        .build()?;

    let pool = pool.clone();

    let mut results = stream::iter(feeds.into_iter())
        .map(|feed| {
            let client = client.clone();
            let pool = pool.clone();
            async move {
                let group_name = feed.group.clone();
                let feed_name = feed.name.clone();

                match fetch_and_parse_feed(&client, &feed.url, Some(&feed.group)).await {
                    Ok(entries) => {
                        let processed = entries.len();
                        match upsert_queue_entries(&pool, entries, Some(feed.group.clone())).await {
                            Ok(_) => FetchRssFeedResult {
                                group: group_name,
                                name: feed_name,
                                processed,
                                error: None,
                            },
                            Err(e) => FetchRssFeedResult {
                                group: group_name,
                                name: feed_name,
                                processed: 0,
                                error: Some(e.to_string()),
                            },
                        }
                    }
                    Err(e) => FetchRssFeedResult {
                        group: group_name,
                        name: feed_name,
                        processed: 0,
                        error: Some(e.to_string()),
                    },
                }
            }
        })
        .buffer_unordered(MAX_CONCURRENT_FEED_REQUESTS)
        .collect::<Vec<_>>()
        .await;

    results.sort_by(|a, b| a.group.cmp(&b.group).then(a.name.cmp(&b.name)));
    let total_processed = results.iter().map(|feed| feed.processed).sum();

    Ok(FetchRssSummary {
        total_processed,
        feeds: results,
    })
}

#[cfg(test)]
mod tests {
    pub mod load_rss {
        use crate::fetch_rss::load_rss_links;

        /// # 検証目的
        /// rss_links.ymlを読み込み、定義済みフィードが存在することを確認する。
        #[test]
        fn rss_linksを読み込める() {
            if std::path::Path::new("rss_links.yml").exists() {
                let result = load_rss_links("rss_links.yml");
                assert!(result.is_ok(), "rss_links.ymlの読み込みに失敗");

                let feeds = result.unwrap();
                assert!(!feeds.is_empty(), "フィードが空");
            }
        }
    }

    pub mod extract_link {
        use feed_rs::model::{Content, Entry};

        use crate::fetch_rss::{extract_link, find_first_url};

        /// # 検証目的
        /// コンテンツ内に含まれるURLを抽出し、末尾の句読点が除去されることを確認する。
        #[test]
        fn コンテンツから_urlを抽出する() {
            let mut entry = Entry::default();
            let mut content = Content::default();
            content.body = Some("テキスト https://example.com/path?a=1) があります".to_string());
            entry.content = Some(content);

            let link = extract_link(&entry);
            assert_eq!(link.as_deref(), Some("https://example.com/path?a=1"));
        }

        /// # 検証目的
        /// 文末に句読点が付いている場合でも適切に除去できることを確認する。
        #[test]
        fn 末尾の句読点を除去する() {
            let url = find_first_url("リンク https://example.com/test). 次");
            assert_eq!(url.as_deref(), Some("https://example.com/test"));
        }
    }

    pub mod parse_feed_content {
        use anyhow::Result;

        use crate::fetch_rss::parse_feed_content;

        /// # 検証目的
        /// RSSドキュメントを解析し、グループや日付のフォールバックが正しく行われることを確認する。
        #[test]
        fn rss文字列を解析できる() -> Result<()> {
            let rss = r#"<?xml version="1.0" encoding="UTF-8"?>
                <rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom">
                  <channel>
                    <title>Example Feed</title>
                    <link>https://feed.example.com</link>
                    <description>Sample</description>
                    <item>
                      <title>Item One</title>
                      <link>https://example.com/one</link>
                      <pubDate>Mon, 13 Oct 2025 12:00:00 GMT</pubDate>
                      <description>本文1</description>
                    </item>
                    <item>
                      <title>Item Two</title>
                      <description>詳細 https://example.com/two.</description>
                      <atom:updated>2025-10-13T13:00:00Z</atom:updated>
                    </item>
                  </channel>
                </rss>
            "#;

            let entries = parse_feed_content(rss.as_bytes(), Some("news"))?;

            assert_eq!(entries.len(), 2);

            let first = &entries[0];
            assert_eq!(first.link, "https://example.com/one");
            assert_eq!(first.title, "Item One");
            assert!(first.pub_date.is_some());
            assert_eq!(first.description, "本文1");
            assert_eq!(first.group.as_deref(), Some("news"));

            let second = &entries[1];
            assert_eq!(second.link, "https://example.com/two");
            assert_eq!(second.title, "Item Two");
            assert_eq!(second.description, "詳細 https://example.com/two.");

            Ok(())
        }
    }

    pub mod upsert_queue_entries {
        use anyhow::Result;
        use chrono::Utc;

        use crate::fetch_rss::upsert_queue_entries;
        use crate::models::NewQueue;
        use crate::test_support::{clear_rss_tables, prepare_test_pool};

        /// # 検証目的
        /// 初回INSERTでレコードが作成され、feed側のgroup指定が適用されることを確認する。
        #[tokio::test]
        async fn 初回挿入で_feed_groupが保存される() -> Result<()> {
            let _lock = crate::test_support::acquire_db_lock().await;
            let pool = prepare_test_pool().await?;

            sqlx::migrate!("./migrations").run(&pool).await?;
            clear_rss_tables(&pool).await?;

            let entries = vec![
                NewQueue {
                    link: "https://example.com/item1".to_string(),
                    title: "Item1".to_string(),
                    pub_date: Some(Utc::now()),
                    description: "本文1".to_string(),
                    group: None,
                },
                NewQueue {
                    link: "https://example.com/item2".to_string(),
                    title: "Item2".to_string(),
                    pub_date: None,
                    description: "本文2".to_string(),
                    group: None,
                },
            ];

            let inserted = upsert_queue_entries(&pool, entries, Some("world".to_string())).await?;
            assert_eq!(inserted, 2);

            let records: Vec<(String, Option<String>)> =
                sqlx::query_as::<_, (String, Option<String>)>(
                    "SELECT link, \"group\" FROM rss.queue ORDER BY link",
                )
                .fetch_all(&pool)
                .await?;

            assert_eq!(records.len(), 2);
            for (_, group) in records {
                assert_eq!(group.as_deref(), Some("world"));
            }

            Ok(())
        }

        /// # 検証目的
        /// 既存リンクに対するUPSERTでタイトルとグループが更新されることを確認する。
        #[tokio::test]
        async fn 重複リンクを更新できる() -> Result<()> {
            let _lock = crate::test_support::acquire_db_lock().await;
            let pool = prepare_test_pool().await?;

            sqlx::migrate!("./migrations").run(&pool).await?;
            clear_rss_tables(&pool).await?;

            let initial = vec![NewQueue {
                link: "https://example.com/item".to_string(),
                title: "Old Title".to_string(),
                pub_date: None,
                description: "Old Desc".to_string(),
                group: None,
            }];

            upsert_queue_entries(&pool, initial, Some("initial".to_string())).await?;

            let updated = vec![NewQueue {
                link: "https://example.com/item".to_string(),
                title: "New Title".to_string(),
                pub_date: None,
                description: "New Desc".to_string(),
                group: Some("entry".to_string()),
            }];

            upsert_queue_entries(&pool, updated, None).await?;

            let row: (String, String) = sqlx::query_as::<_, (String, String)>(
                "SELECT title, description FROM rss.queue WHERE link = $1",
            )
            .bind("https://example.com/item")
            .fetch_one(&pool)
            .await?;
            assert_eq!(row.0, "New Title");
            assert_eq!(row.1, "New Desc");

            let group: Option<String> =
                sqlx::query_scalar("SELECT \"group\" FROM rss.queue WHERE link = $1")
                    .bind("https://example.com/item")
                    .fetch_one(&pool)
                    .await?;
            assert_eq!(group.as_deref(), Some("entry"));

            Ok(())
        }
    }

    pub mod execute_fetch_rss_tests {
        use anyhow::Result;
        use std::time::{Duration, Instant};
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        use crate::fetch_rss::execute_fetch_rss;
        use crate::test_support::{clear_rss_tables, create_temp_yaml, prepare_test_pool};

        /// # 検証目的
        /// フィード取得が失敗した場合にサマリへエラーが記録されることを確認する。
        #[tokio::test]
        async fn フィード失敗時にエラーが記録される() -> Result<()> {
            let _lock = crate::test_support::acquire_db_lock().await;
            let pool = prepare_test_pool().await?;

            sqlx::migrate!("./migrations").run(&pool).await?;
            clear_rss_tables(&pool).await?;

            let server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/feed"))
                .respond_with(ResponseTemplate::new(500).set_body_string("error"))
                .mount(&server)
                .await;

            let temp_file =
                create_temp_yaml(&format!("test:\n  failure: {url}/feed", url = server.uri()))?;

            let summary =
                execute_fetch_rss(&pool, temp_file.path().to_string_lossy().as_ref()).await?;

            assert_eq!(summary.total_processed, 0);
            assert_eq!(summary.feeds.len(), 1);
            let feed = &summary.feeds[0];
            assert_eq!(feed.processed, 0);
            assert!(feed.error.is_some(), "エラーが記録されていない");

            Ok(())
        }

        /// # 検証目的
        /// 複数フィードを並列に取得できることで全体時間が短縮されることを確認する。
        #[tokio::test]
        async fn 複数フィードを並列取得する() -> Result<()> {
            let _lock = crate::test_support::acquire_db_lock().await;
            let pool = prepare_test_pool().await?;

            sqlx::migrate!("./migrations").run(&pool).await?;
            clear_rss_tables(&pool).await?;

            let server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/feed1"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_string(simple_rss_entry("https://example.com/one"))
                        .set_delay(Duration::from_millis(500)),
                )
                .mount(&server)
                .await;

            Mock::given(method("GET"))
                .and(path("/feed2"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_string(simple_rss_entry("https://example.com/two"))
                        .set_delay(Duration::from_millis(500)),
                )
                .mount(&server)
                .await;

            let temp_file = create_temp_yaml(&format!(
                "parallel:\n  first: {url}/feed1\n  second: {url}/feed2\n",
                url = server.uri()
            ))?;

            let started = Instant::now();
            let summary =
                execute_fetch_rss(&pool, temp_file.path().to_string_lossy().as_ref()).await?;
            let elapsed = started.elapsed();

            assert_eq!(summary.total_processed, 2);
            assert!(
                elapsed < Duration::from_millis(900),
                "取得が直列実行された可能性があります (elapsed = {:?})",
                elapsed
            );

            Ok(())
        }

        fn simple_rss_entry(link: &str) -> String {
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Mock Feed</title>
    <item>
      <title>Title</title>
      <link>{link}</link>
      <description>desc</description>
      <pubDate>Mon, 13 Oct 2025 12:00:00 GMT</pubDate>
    </item>
  </channel>
</rss>
"#
            )
        }
    }
}

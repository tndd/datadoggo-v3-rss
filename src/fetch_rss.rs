use std::collections::BTreeMap;
use std::fs;

use anyhow::Result;
use feed_rs::{model::Entry, parser};
use once_cell::sync::Lazy;
use regex::Regex;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::{NewQueue, RssFeedSource, RssLinks};

static URL_PATTERN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"https?://[^\s\"'<>()]+"#).expect("URL正規表現のコンパイルに失敗"));

/// rss_links.ymlを読み込む
pub fn load_rss_links(path: &str) -> Result<Vec<RssFeedSource>> {
    let content = fs::read_to_string(path)?;
    let links: RssLinks = serde_yaml::from_str(&content)?;
    Ok(links.into_sources())
}

/// RSSフィードを取得してパース
pub async fn fetch_and_parse_feed(url: &str, group: Option<&str>) -> Result<Vec<NewQueue>> {
    let response = reqwest::get(url).await?;
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
pub async fn run(pool: PgPool) -> Result<()> {
    println!("rss_links.ymlを読み込み中...");
    let feeds = load_rss_links("rss_links.yml")?;

    let mut grouped: BTreeMap<String, Vec<RssFeedSource>> = BTreeMap::new();
    for feed in feeds {
        grouped.entry(feed.group.clone()).or_default().push(feed);
    }

    if grouped.is_empty() {
        println!("登録されているRSSフィードがありません");
        return Ok(());
    }

    let mut total_count = 0;

    for (group_name, feeds) in grouped {
        println!("\nグループ: {}", group_name);

        for feed in feeds {
            print!("  - {} を取得中... ", feed.name);

            match fetch_and_parse_feed(&feed.url, Some(&feed.group)).await {
                Ok(entries) => {
                    let count = entries.len();
                    match upsert_queue_entries(&pool, entries, Some(feed.group.clone())).await {
                        Ok(_) => {
                            println!("✓ {}件の記事を処理", count);
                            total_count += count;
                        }
                        Err(e) => {
                            println!("✗ DB保存エラー: {}", e);
                        }
                    }
                }
                Err(e) => {
                    println!("✗ 取得エラー: {}", e);
                }
            }
        }
    }

    println!("\n合計: {}件の記事を処理しました", total_count);
    Ok(())
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
        use sqlx::PgPool;

        use crate::fetch_rss::upsert_queue_entries;
        use crate::models::NewQueue;

        async fn prepare_pool() -> Option<PgPool> {
            match std::env::var("TEST_DATABASE_URL") {
                Ok(url) => match PgPool::connect(&url).await {
                    Ok(pool) => Some(pool),
                    Err(e) => {
                        eprintln!("TEST_DATABASE_URLへ接続できないためスキップ: {}", e);
                        None
                    }
                },
                Err(_) => {
                    eprintln!("TEST_DATABASE_URLが未設定のためスキップ");
                    None
                }
            }
        }

        /// # 検証目的
        /// 初回INSERTでレコードが作成され、feed側のgroup指定が適用されることを確認する。
        #[tokio::test]
        async fn 初回挿入で_feed_groupが保存される() -> Result<()> {
            let Some(pool) = prepare_pool().await else {
                return Ok(());
            };

            sqlx::migrate!("./migrations").run(&pool).await?;
            sqlx::query("TRUNCATE rss.queue CASCADE").execute(&pool).await?;

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

            let records: Vec<(String, Option<String>)> = sqlx::query_as::<_, (String, Option<String>)>(
                "SELECT link, \"group\" FROM rss.queue ORDER BY link"
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
            let Some(pool) = prepare_pool().await else {
                return Ok(());
            };

            sqlx::migrate!("./migrations").run(&pool).await?;
            sqlx::query("TRUNCATE rss.queue CASCADE").execute(&pool).await?;

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
                "SELECT title, description FROM rss.queue WHERE link = $1"
            )
            .bind("https://example.com/item")
            .fetch_one(&pool)
            .await?;
            assert_eq!(row.0, "New Title");
            assert_eq!(row.1, "New Desc");

            let group: Option<String> = sqlx::query_scalar(
                "SELECT \"group\" FROM rss.queue WHERE link = $1"
            )
            .bind("https://example.com/item")
            .fetch_one(&pool)
            .await?;
            assert_eq!(group.as_deref(), Some("entry"));

            Ok(())
        }
    }
}

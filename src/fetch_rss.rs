use std::collections::BTreeMap;
use std::fs;

use anyhow::Result;
use feed_rs::{model::Entry, parser};
use once_cell::sync::Lazy;
use regex::Regex;
use sqlx::PgPool;

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
    let feed = parser::parse(&content[..])?;

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

fn extract_link(entry: &Entry) -> Option<String> {
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

fn find_first_url(text: &str) -> Option<String> {
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

        sqlx::query(
            r#"
            INSERT INTO rss.queue (link, title, pub_date, description, "group")
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (link)
            DO UPDATE SET
                title = EXCLUDED.title,
                pub_date = EXCLUDED.pub_date,
                description = EXCLUDED.description,
                "group" = EXCLUDED."group",
                updated_at = NOW()
            "#,
        )
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
        use super::super::load_rss_links;

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

        use super::super::{extract_link, find_first_url};

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
}

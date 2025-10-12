use anyhow::Result;
use feed_rs::parser;
use sqlx::PgPool;
use std::fs;
use crate::models::{NewQueue, RssLinks};

/// rss_links.ymlを読み込む
pub fn load_rss_links(path: &str) -> Result<RssLinks> {
    let content = fs::read_to_string(path)?;
    let links: RssLinks = serde_yaml::from_str(&content)?;
    Ok(links)
}

/// RSSフィードを取得してパース
pub async fn fetch_and_parse_feed(url: &str) -> Result<Vec<NewQueue>> {
    let response = reqwest::get(url).await?;
    let content = response.bytes().await?;
    let feed = parser::parse(&content[..])?;

    let mut entries = Vec::new();

    for entry in feed.entries {
        let link = entry
            .links
            .first()
            .map(|l| l.href.clone())
            .unwrap_or_default();

        let title = entry
            .title
            .map(|t| t.content)
            .unwrap_or_else(|| "No title".to_string());

        let pub_date = entry.published.or(entry.updated);

        let description = entry
            .summary
            .map(|t| t.content)
            .or_else(|| {
                entry.content.and_then(|c| c.body)
            })
            .unwrap_or_else(|| String::new());

        entries.push(NewQueue {
            link,
            title,
            pub_date,
            description,
            group: None,
        });
    }

    Ok(entries)
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
            "#
        )
        .bind(&entry.link)
        .bind(&entry.title)
        .bind(&entry.pub_date)
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
    let rss_links = load_rss_links("rss_links.yml")?;

    let mut total_count = 0;

    for (group_name, feeds) in rss_links.groups {
        println!("\nグループ: {}", group_name);

        for (feed_name, url) in feeds {
            print!("  - {} を取得中... ", feed_name);

            match fetch_and_parse_feed(&url).await {
                Ok(entries) => {
                    let count = entries.len();
                    match upsert_queue_entries(&pool, entries, Some(group_name.clone())).await {
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
    use super::*;

    #[test]
    fn test_load_rss_links() {
        // rss_links.ymlが存在する場合のみテスト
        if std::path::Path::new("rss_links.yml").exists() {
            let result = load_rss_links("rss_links.yml");
            assert!(result.is_ok(), "rss_links.ymlの読み込みに失敗");

            let links = result.unwrap();
            assert!(!links.groups.is_empty(), "グループが空");
        }
    }

    #[tokio::test]
    async fn test_fetch_and_parse_feed() {
        // BBC RSSフィードでテスト（実際の通信が発生）
        let url = "https://feeds.bbci.co.uk/news/rss.xml";
        let result = fetch_and_parse_feed(url).await;

        // ネットワークエラーは許容
        if let Ok(entries) = result {
            assert!(!entries.is_empty(), "記事が取得できませんでした");
        }
    }
}

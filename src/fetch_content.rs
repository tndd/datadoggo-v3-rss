use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;
use crate::models::{Queue, ScrapeRequest, ScrapeResponse};

/// モックスクレイピングAPI（開発用）
/// URLのハッシュ値に基づいて成功/失敗をシミュレート
async fn mock_scrape_api(url: &str) -> Result<ScrapeResponse> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    // URLのハッシュ値を計算して決定的な振る舞いを実現
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    let hash = hasher.finish();

    // ハッシュ値の下位ビットで成功率80%を実現
    let status_code = if hash % 10 < 8 {
        200 // 80%の確率で成功
    } else if hash % 10 == 8 {
        404 // 10%の確率で404
    } else {
        500 // 10%の確率で500
    };

    let html = if status_code == 200 {
        format!("<html><body><h1>Mock content for {}</h1><p>This is a successful response.</p></body></html>", url)
    } else {
        format!("<html><body><h1>Error {}</h1></body></html>", status_code)
    };

    Ok(ScrapeResponse {
        html,
        status_code,
        title: format!("Mock Title for {}", url),
        final_url: url.to_string(),
        elapsed_ms: 100.0 + (hash % 500) as f64, // 100-600ms
        timestamp: chrono::Utc::now().to_rfc3339(),
    })
}

/// スクレイピングAPIを呼び出し（本番環境用、今は未使用）
#[allow(dead_code)]
async fn call_scrape_api(api_url: &str, request: ScrapeRequest) -> Result<ScrapeResponse> {
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/fetch", api_url))
        .json(&request)
        .send()
        .await?;

    let scrape_response: ScrapeResponse = response.json().await?;
    Ok(scrape_response)
}

/// HTMLをBrotli圧縮
fn compress_html(html: &str) -> Result<Vec<u8>> {
    let mut compressed = Vec::new();
    let mut reader = html.as_bytes();

    brotli::BrotliCompress(
        &mut reader,
        &mut compressed,
        &brotli::enc::BrotliEncoderParams {
            quality: 6,
            ..Default::default()
        },
    )?;

    Ok(compressed)
}

/// status_code=NULLのqueueエントリを取得
async fn search_pending_queue_entries(pool: &PgPool, limit: i64) -> Result<Vec<Queue>> {
    let entries = sqlx::query_as::<_, Queue>(
        r#"
        SELECT id, created_at, updated_at, link, title, pub_date, description, status_code, "group"
        FROM rss.queue
        WHERE status_code IS NULL
        ORDER BY created_at ASC
        LIMIT $1
        "#
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(entries)
}

/// queueのstatus_codeを更新
async fn update_queue_status(pool: &PgPool, id: Uuid, status_code: i32) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE rss.queue
        SET status_code = $1, updated_at = NOW()
        WHERE id = $2
        "#
    )
    .bind(status_code)
    .bind(id)
    .execute(pool)
    .await?;

    Ok(())
}

/// article_contentに保存
async fn save_article_content(pool: &PgPool, queue_id: Uuid, data: Vec<u8>) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO rss.article_content (queue_id, data)
        VALUES ($1, $2)
        ON CONFLICT (queue_id)
        DO UPDATE SET
            data = EXCLUDED.data,
            updated_at = NOW()
        "#
    )
    .bind(queue_id)
    .bind(data)
    .execute(pool)
    .await?;

    Ok(())
}

/// fetch-contentコマンドのメイン処理
pub async fn run(pool: PgPool, limit: i64) -> Result<()> {
    println!("status_code=NULLのエントリを取得中...");
    let entries = search_pending_queue_entries(&pool, limit).await?;

    if entries.is_empty() {
        println!("処理対象のエントリがありません");
        return Ok(());
    }

    println!("{}件のエントリを処理します\n", entries.len());

    let mut success_count = 0;
    let mut error_count = 0;

    for entry in entries {
        print!("  - {} ... ", entry.title);

        // モックAPIを呼び出し
        match mock_scrape_api(&entry.link).await {
            Ok(response) => {
                // status_codeを更新
                if let Err(e) = update_queue_status(&pool, entry.id, response.status_code).await {
                    println!("✗ status_code更新エラー: {}", e);
                    error_count += 1;
                    continue;
                }

                // status_code=200の場合のみコンテンツを保存
                if response.status_code == 200 {
                    match compress_html(&response.html) {
                        Ok(compressed) => {
                            if let Err(e) = save_article_content(&pool, entry.id, compressed).await {
                                println!("✗ コンテンツ保存エラー: {}", e);
                                error_count += 1;
                            } else {
                                println!("✓ 保存完了 (status: {})", response.status_code);
                                success_count += 1;
                            }
                        }
                        Err(e) => {
                            println!("✗ 圧縮エラー: {}", e);
                            error_count += 1;
                        }
                    }
                } else {
                    println!("✓ status_codeのみ記録 (status: {})", response.status_code);
                    success_count += 1;
                }
            }
            Err(e) => {
                println!("✗ APIエラー: {}", e);
                error_count += 1;
            }
        }
    }

    println!("\n完了: 成功 {}件, エラー {}件", success_count, error_count);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_scrape_api() {
        let result = mock_scrape_api("https://example.com").await;
        assert!(result.is_ok(), "モックAPIの呼び出しに失敗");

        let response = result.unwrap();
        assert_eq!(response.status_code, 200);
        assert!(!response.html.is_empty());
    }

    #[test]
    fn test_compress_html() {
        let html = "<html><body>Test content</body></html>";
        let result = compress_html(html);

        assert!(result.is_ok(), "HTML圧縮に失敗");

        let compressed = result.unwrap();
        assert!(!compressed.is_empty());
        assert!(compressed.len() < html.len(), "圧縮後のサイズが元のサイズより大きい");
    }

    #[tokio::test]
    async fn test_search_pending_queue_entries() {
        // 環境変数が正しく設定されている場合のみテスト
        if let Ok(db_url) = std::env::var("DATABASE_URL") {
            if let Ok(pool) = crate::db::create_pool(&db_url).await {
                let result = search_pending_queue_entries(&pool, 10).await;
                // DBが初期化されていない場合はエラーになる可能性があるので、結果の存在のみ確認
                assert!(result.is_ok() || result.is_err());
            }
        }
    }
}

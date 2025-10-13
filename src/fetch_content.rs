use crate::models::{Queue, ScrapeRequest, ScrapeResponse};
use anyhow::{Context, Result};
use reqwest::Client;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

const DEFAULT_TIMEOUT_SECS: u64 = 15;

enum ScrapeResult {
    Success(ScrapeResponse),
    HttpError { status_code: i32 },
}

/// fetch-contentコマンドのメイン処理
pub async fn run(pool: PgPool, limit: i64, api_url: &str) -> Result<()> {
    println!("status_code=NULLのエントリを取得中...");
    let entries = search_pending_queue_entries(&pool, limit).await?;

    if entries.is_empty() {
        println!("処理対象のエントリがありません");
        return Ok(());
    }

    println!("{}件のエントリを処理します\n", entries.len());

    let client = Client::new();
    let mut saved_count = 0;
    let mut status_only_count = 0;
    let mut error_count = 0;

    for entry in entries {
        print!("  - {} ... ", entry.title);

        let request = ScrapeRequest {
            url: entry.link.clone(),
            wait_for_selector: None,
            timeout: Some(DEFAULT_TIMEOUT_SECS),
        };

        match call_scrape_api(&client, api_url, &request).await {
            Ok(ScrapeResult::Success(response)) => {
                if response.status_code == 200 {
                    match persist_success(&pool, entry.id, &response.html, response.status_code)
                        .await
                    {
                        Ok(_) => {
                            println!("✓ 保存完了 (status: {})", response.status_code);
                            saved_count += 1;
                        }
                        Err(e) => {
                            println!("✗ 保存エラー: {}", e);
                            error_count += 1;
                        }
                    }
                } else {
                    match persist_status_only(&pool, entry.id, response.status_code).await {
                        Ok(_) => {
                            println!("✓ status_codeのみ記録 (status: {})", response.status_code);
                            status_only_count += 1;
                        }
                        Err(e) => {
                            println!("✗ status_code更新エラー: {}", e);
                            error_count += 1;
                        }
                    }
                }
            }
            Ok(ScrapeResult::HttpError { status_code }) => {
                match persist_status_only(&pool, entry.id, status_code).await {
                    Ok(_) => {
                        println!("✓ HTTP{}を記録 (statusのみ)", status_code);
                        status_only_count += 1;
                    }
                    Err(e) => {
                        println!("✗ status_code更新エラー: {}", e);
                        error_count += 1;
                    }
                }
            }
            Err(e) => {
                println!("✗ APIエラー: {}", e);
                error_count += 1;
            }
        }
    }

    println!(
        "\n完了: 本文保存 {}件, statusのみ {}件, エラー {}件",
        saved_count, status_only_count, error_count
    );

    Ok(())
}

/// スクレイピングAPIを呼び出す
async fn call_scrape_api(
    client: &Client,
    api_url: &str,
    request: &ScrapeRequest,
) -> Result<ScrapeResult> {
    let endpoint = format!("{}/fetch", api_url.trim_end_matches('/'));
    let response = client.post(endpoint).json(request).send().await?;

    let status = response.status();
    let bytes = response.bytes().await?;

    if status.is_success() {
        let scrape_response: ScrapeResponse = serde_json::from_slice(&bytes)
            .context("スクレイピングAPIレスポンスのJSONデコードに失敗")?;
        Ok(ScrapeResult::Success(scrape_response))
    } else {
        Ok(ScrapeResult::HttpError {
            status_code: status.as_u16() as i32,
        })
    }
}

/// HTMLをBrotli圧縮
pub(crate) fn compress_html(html: &str) -> Result<Vec<u8>> {
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
        "#,
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(entries)
}

/// queueのstatus_codeを更新
async fn update_queue_status(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    status_code: i32,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE rss.queue
        SET status_code = $1, updated_at = NOW()
        WHERE id = $2
        "#,
    )
    .bind(status_code)
    .bind(id)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// article_contentに保存
async fn save_article_content(
    tx: &mut Transaction<'_, Postgres>,
    queue_id: Uuid,
    data: &[u8],
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO rss.article_content (queue_id, data)
        VALUES ($1, $2)
        ON CONFLICT (queue_id)
        DO UPDATE SET
            data = EXCLUDED.data,
            updated_at = NOW()
        "#,
    )
    .bind(queue_id)
    .bind(data)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// 取得結果が200のときの保存処理
async fn persist_success(
    pool: &PgPool,
    queue_id: Uuid,
    html: &str,
    status_code: i32,
) -> Result<()> {
    let compressed = compress_html(html)?;
    let mut tx = pool.begin().await?;

    save_article_content(&mut tx, queue_id, &compressed).await?;
    update_queue_status(&mut tx, queue_id, status_code).await?;

    tx.commit().await?;
    Ok(())
}

/// 取得結果が非200のときの更新処理
async fn persist_status_only(pool: &PgPool, queue_id: Uuid, status_code: i32) -> Result<()> {
    let mut tx = pool.begin().await?;
    update_queue_status(&mut tx, queue_id, status_code).await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    pub mod run {
        use std::io::{Cursor, Read};

        use anyhow::Result;
        use brotli::Decompressor;
        use chrono::Utc;
        use serde_json::json;
        use sqlx::PgPool;
        use uuid::Uuid;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        use crate::fetch_content::run;
        use crate::models::ArticleContent;

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
                    eprintln!("TEST_DATABASE_URLが設定されていないためスキップ");
                    None
                }
            }
        }

        /// # 検証目的
        /// ステータス200時に本文を圧縮保存し、status_codeを200へ更新できることを確認する。
        #[tokio::test]
        async fn 保存成功で記事が圧縮保存される() -> Result<()> {
            let Some(pool) = prepare_pool().await else {
                return Ok(());
            };

            sqlx::migrate!("./migrations").run(&pool).await?;

            let server = MockServer::start().await;
            let html_body = "<html><body><p>ok</p></body></html>";

            Mock::given(method("POST"))
                .and(path("/fetch"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "html": html_body,
                    "status_code": 200,
                    "title": "Mock Title",
                    "final_url": "https://example.com/",
                    "elapsed_ms": 123.4,
                    "timestamp": Utc::now().to_rfc3339(),
                })))
                .mount(&server)
                .await;

            let queue_id: Uuid = sqlx::query_scalar(
                r#"
                INSERT INTO rss.queue (link, title, description)
                VALUES ($1, $2, $3)
                RETURNING id
                "#,
            )
            .bind("https://example.com")
            .bind("テストタイトル")
            .bind("テスト説明")
            .fetch_one(&pool)
            .await?;

            run(pool.clone(), 10, &server.uri()).await?;

            let status: Option<i32> =
                sqlx::query_scalar("SELECT status_code FROM rss.queue WHERE id = $1")
                    .bind(queue_id)
                    .fetch_one(&pool)
                    .await?;
            assert_eq!(status, Some(200));

            let stored: ArticleContent = sqlx::query_as(
                "SELECT queue_id, created_at, updated_at, data FROM rss.article_content WHERE queue_id = $1",
            )
            .bind(queue_id)
            .fetch_one(&pool)
            .await?;

            assert_eq!(stored.queue_id, queue_id);
            assert!(stored.created_at <= Utc::now());
            assert!(stored.updated_at <= Utc::now());

            let mut decompressor = Decompressor::new(Cursor::new(stored.data), 4096);
            let mut decompressed = String::new();
            decompressor.read_to_string(&mut decompressed)?;
            assert_eq!(decompressed, html_body);

            Ok(())
        }

        /// # 検証目的
        /// ステータス200以外では本文を保存せず、status_codeのみを更新することを確認する。
        #[tokio::test]
        async fn 非200はstatusのみを記録する() -> Result<()> {
            let Some(pool) = prepare_pool().await else {
                return Ok(());
            };

            sqlx::migrate!("./migrations").run(&pool).await?;

            let server = MockServer::start().await;

            Mock::given(method("POST"))
                .and(path("/fetch"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "html": "",
                    "status_code": 404,
                    "title": "Not Found",
                    "final_url": "https://example.com/missing",
                    "elapsed_ms": 321.0,
                    "timestamp": Utc::now().to_rfc3339(),
                })))
                .mount(&server)
                .await;

            let queue_id: Uuid = sqlx::query_scalar(
                r#"
                INSERT INTO rss.queue (link, title, description)
                VALUES ($1, $2, $3)
                RETURNING id
                "#,
            )
            .bind("https://example.com/missing")
            .bind("テストタイトル")
            .bind("テスト説明")
            .fetch_one(&pool)
            .await?;

            run(pool.clone(), 10, &server.uri()).await?;

            let status: Option<i32> =
                sqlx::query_scalar("SELECT status_code FROM rss.queue WHERE id = $1")
                    .bind(queue_id)
                    .fetch_one(&pool)
                    .await?;
            assert_eq!(status, Some(404));

            let exists: Option<ArticleContent> = sqlx::query_as(
                "SELECT queue_id, created_at, updated_at, data FROM rss.article_content WHERE queue_id = $1",
            )
            .bind(queue_id)
            .fetch_optional(&pool)
            .await?;
            assert!(exists.is_none());

            Ok(())
        }

        /// # 検証目的
        /// スクレイピングAPIがHTTPエラーを返した場合でもstatus_codeを保存することを確認する。
        #[tokio::test]
        async fn httpエラーでもstatusを記録する() -> Result<()> {
            let Some(pool) = prepare_pool().await else {
                return Ok(());
            };

            sqlx::migrate!("./migrations").run(&pool).await?;

            let server = MockServer::start().await;

            Mock::given(method("POST"))
                .and(path("/fetch"))
                .respond_with(
                    ResponseTemplate::new(503).set_body_string("upstream error"),
                )
                .mount(&server)
                .await;

            let queue_id: Uuid = sqlx::query_scalar(
                r#"
                INSERT INTO rss.queue (link, title, description)
                VALUES ($1, $2, $3)
                RETURNING id
                "#,
            )
            .bind("https://example.com/503")
            .bind("テストタイトル503")
            .bind("テスト説明503")
            .fetch_one(&pool)
            .await?;

            run(pool.clone(), 10, &server.uri()).await?;

            let status: Option<i32> =
                sqlx::query_scalar("SELECT status_code FROM rss.queue WHERE id = $1")
                    .bind(queue_id)
                    .fetch_one(&pool)
                    .await?;
            assert_eq!(status, Some(503));

            let exists: Option<ArticleContent> = sqlx::query_as(
                "SELECT queue_id, created_at, updated_at, data FROM rss.article_content WHERE queue_id = $1",
            )
            .bind(queue_id)
            .fetch_optional(&pool)
            .await?;
            assert!(exists.is_none());

            Ok(())
        }
    }

    pub mod compress_html {
        use std::io::{Cursor, Read};

        use brotli::Decompressor;

        use crate::fetch_content::compress_html;

        /// # 検証目的
        /// Brotli圧縮したHTMLを無損失で展開できることを確認する。
        #[test]
        fn 圧縮は可逆である() {
            let html = "<html><body>テスト</body></html>";
            let compressed = compress_html(html).expect("Brotli圧縮に失敗");

            let mut decompressor = Decompressor::new(Cursor::new(compressed), 4096);
            let mut decompressed = String::new();
            decompressor
                .read_to_string(&mut decompressed)
                .expect("Brotli展開に失敗");

            assert_eq!(decompressed, html);
        }
    }
}

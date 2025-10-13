use std::net::{IpAddr, SocketAddr};

use anyhow::Result;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;

use crate::fetch_content::{execute_fetch_content, FetchContentSummary};
use crate::fetch_rss::{execute_fetch_rss, FetchRssSummary};

/// APIサーバで共有する状態
#[derive(Clone)]
pub struct ApiState {
    pub pool: PgPool,
    pub scraping_api_url: String,
    pub rss_links_path: String,
}

impl ApiState {
    pub fn new(pool: PgPool, scraping_api_url: String, rss_links_path: String) -> Self {
        Self {
            pool,
            scraping_api_url,
            rss_links_path,
        }
    }
}

/// APIサーバを起動する
pub async fn serve(state: ApiState, host: IpAddr, port: u16) -> Result<()> {
    let addr = SocketAddr::from((host, port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let router = build_router(state);
    axum::serve(listener, router).await?;
    Ok(())
}

/// ルータを構築する
pub fn build_router(state: ApiState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/fetch-rss", post(fetch_rss_handler))
        .route("/api/fetch-content", post(fetch_content_handler))
        .with_state(state)
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({ "status": "ok" })))
}

#[derive(Debug, Deserialize)]
struct FetchContentRequest {
    limit: Option<i64>,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    message: String,
}

type ApiResult<T> = Result<T, (StatusCode, Json<ErrorResponse>)>;

fn internal_error<E: std::fmt::Display>(err: E) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            message: err.to_string(),
        }),
    )
}

fn bad_request<E: std::fmt::Display>(err: E) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse {
            message: err.to_string(),
        }),
    )
}

async fn fetch_rss_handler(State(state): State<ApiState>) -> ApiResult<Json<FetchRssSummary>> {
    execute_fetch_rss(&state.pool, &state.rss_links_path)
        .await
        .map(Json)
        .map_err(internal_error)
}

async fn fetch_content_handler(
    State(state): State<ApiState>,
    Json(payload): Json<FetchContentRequest>,
) -> ApiResult<Json<FetchContentSummary>> {
    let limit = payload.limit.unwrap_or(100);
    if limit <= 0 {
        return Err(bad_request("limitは1以上で指定してください"));
    }

    execute_fetch_content(&state.pool, limit, &state.scraping_api_url)
        .await
        .map(Json)
        .map_err(internal_error)
}

#[cfg(test)]
mod tests {
    pub mod fetch_rss_endpoint {
        use std::fs;
        use std::io::Write;

        use anyhow::Result;
        use axum::body::{to_bytes, Body};
        use axum::http::{Request, StatusCode};
        use serde_json::Value;
        use sqlx::PgPool;
        use tower::ServiceExt;
        use uuid::Uuid;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        use crate::api::{build_router, ApiState};

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

        fn create_temp_rss_links(server_url: &str) -> Result<String> {
            let path = std::env::temp_dir().join(format!("rss_links_test_{}.yml", Uuid::new_v4()));
            let mut file = fs::File::create(&path)?;
            writeln!(
                file,
                "test:
  sample: {url}/feed",
                url = server_url
            )?;
            Ok(path.to_string_lossy().to_string())
        }

        /// # 検証目的
        /// API経由でRSS取得処理が実行され、結果がJSONで返ることを確認する。
        #[tokio::test]
        async fn rssを取得するエンドポイントが動作する() -> Result<()> {
            let _lock = crate::test_support::acquire_db_lock().await;
            let Some(pool) = prepare_pool().await else {
                return Ok(());
            };

            sqlx::migrate!("./migrations").run(&pool).await?;
            sqlx::query("TRUNCATE rss.article_content CASCADE")
                .execute(&pool)
                .await?;
            sqlx::query("TRUNCATE rss.queue CASCADE")
                .execute(&pool)
                .await?;

            let server = MockServer::start().await;
            let rss_body = r#"<?xml version="1.0" encoding="UTF-8"?>
                <rss version="2.0">
                  <channel>
                    <title>Test</title>
                    <item>
                      <title>Item</title>
                      <link>https://example.com/item</link>
                      <description>desc</description>
                      <pubDate>Mon, 13 Oct 2025 12:00:00 GMT</pubDate>
                    </item>
                  </channel>
                </rss>
            "#;

            Mock::given(method("GET"))
                .and(path("/feed"))
                .respond_with(ResponseTemplate::new(200).set_body_string(rss_body))
                .mount(&server)
                .await;

            let rss_links_path = create_temp_rss_links(&server.uri())?;

            let state = ApiState::new(pool.clone(), server.uri(), rss_links_path.clone());
            let app = build_router(state);

            let response = app
                .oneshot(Request::post("/api/fetch-rss").body(Body::empty()).unwrap())
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);

            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let value: Value = serde_json::from_slice(&bytes)?;
            assert_eq!(value["total_processed"].as_u64(), Some(1));

            fs::remove_file(rss_links_path)?;
            Ok(())
        }
    }

    pub mod fetch_content_endpoint {
        use std::io::{Cursor, Read};

        use anyhow::Result;
        use axum::body::{to_bytes, Body};
        use axum::http::{Request, StatusCode};
        use brotli::Decompressor;
        use serde_json::json;
        use sqlx::PgPool;
        use tower::ServiceExt;
        use uuid::Uuid;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        use crate::api::{build_router, ApiState};
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
        /// API経由でコンテンツ取得処理を実行し、本文保存とレスポンス内容を確認する。
        #[tokio::test]
        async fn コンテンツ取得apiが成功する() -> Result<()> {
            let _lock = crate::test_support::acquire_db_lock().await;
            let Some(pool) = prepare_pool().await else {
                return Ok(());
            };

            sqlx::migrate!("./migrations").run(&pool).await?;
            sqlx::query("TRUNCATE rss.article_content CASCADE")
                .execute(&pool)
                .await?;
            sqlx::query("TRUNCATE rss.queue CASCADE")
                .execute(&pool)
                .await?;

            let server = MockServer::start().await;
            let html_body = "<html><body>api ok</body></html>";

            Mock::given(method("POST"))
                .and(path("/fetch"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "html": html_body,
                    "status_code": 200,
                    "title": "API Title",
                    "final_url": "https://example.com/api",
                    "elapsed_ms": 10.0,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                })))
                .mount(&server)
                .await;

            let queue_id = Uuid::new_v4();
            sqlx::query(
                r#"
                INSERT INTO rss.queue (id, link, title, description)
                VALUES ($1, $2, $3, $4)
                "#,
            )
            .bind(queue_id)
            .bind("https://example.com/api")
            .bind("APIテスト")
            .bind("API説明")
            .execute(&pool)
            .await?;

            let state = ApiState::new(pool.clone(), server.uri(), "rss_links.yml".to_string());
            let app = build_router(state);

            let response = app
                .oneshot(
                    Request::post("/api/fetch-content")
                        .header("content-type", "application/json")
                        .body(Body::from("{\"limit\":1}"))
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);

            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let value: serde_json::Value = serde_json::from_slice(&bytes)?;
            assert_eq!(value["saved_count"].as_u64(), Some(1));
            assert_eq!(value["status_only_count"].as_u64(), Some(0));

            let stored: ArticleContent = sqlx::query_as(
                "SELECT queue_id, created_at, updated_at, data FROM rss.article_content WHERE queue_id = $1",
            )
            .bind(queue_id)
            .fetch_one(&pool)
            .await?;

            let mut decompressor = Decompressor::new(Cursor::new(stored.data), 4096);
            let mut decompressed = String::new();
            decompressor.read_to_string(&mut decompressed)?;
            assert_eq!(decompressed, html_body);

            Ok(())
        }
    }
}

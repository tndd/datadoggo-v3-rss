use std::net::{IpAddr, SocketAddr};

use anyhow::Result;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;
use tracing::warn;

use crate::articles::{find_article_cursor, search_articles_window};
use crate::fetch_content::{execute_fetch_content, FetchContentSummary};
use crate::fetch_rss::{execute_fetch_rss, FetchRssSummary};
use crate::webhook;

const MAX_LIMIT: i64 = 500;
const UNSPECIFIED_LIMIT: i64 = 500;
pub(crate) const MAX_RESPONSE_BYTES: usize = 50 * 1024 * 1024;

/// APIサーバで共有する状態
#[derive(Clone)]
pub struct ApiState {
    pub pool: PgPool,
    pub scraping_api_url: String,
    pub rss_links_path: String,
    pub webhook_url: Option<String>,
}

impl ApiState {
    pub fn new(
        pool: PgPool,
        scraping_api_url: String,
        rss_links_path: String,
        webhook_url: Option<String>,
    ) -> Self {
        Self {
            pool,
            scraping_api_url,
            rss_links_path,
            webhook_url,
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
        .route("/api/articles", get(list_articles_handler))
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
    code: String,
    message: String,
}

type ApiResult<T> = Result<T, (StatusCode, Json<ErrorResponse>)>;

fn error_response(
    status: StatusCode,
    code: &'static str,
    message: impl std::fmt::Display,
) -> (StatusCode, Json<ErrorResponse>) {
    (
        status,
        Json(ErrorResponse {
            code: code.to_string(),
            message: message.to_string(),
        }),
    )
}

fn internal_error<E: std::fmt::Display>(err: E) -> (StatusCode, Json<ErrorResponse>) {
    error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", err)
}

fn bad_request(
    code: &'static str,
    message: impl std::fmt::Display,
) -> (StatusCode, Json<ErrorResponse>) {
    error_response(StatusCode::BAD_REQUEST, code, message)
}

async fn fetch_rss_handler(State(state): State<ApiState>) -> ApiResult<Json<FetchRssSummary>> {
    let summary = execute_fetch_rss(&state.pool, &state.rss_links_path)
        .await
        .map_err(internal_error)?;

    if let Err(e) = webhook::notify_fetch_rss(state.webhook_url.as_deref(), &summary, "api").await {
        warn!(error = %e, "Webhook送信に失敗しました(fetch-rss)");
    }

    Ok(Json(summary))
}

async fn fetch_content_handler(
    State(state): State<ApiState>,
    Json(payload): Json<FetchContentRequest>,
) -> ApiResult<Json<FetchContentSummary>> {
    let limit = payload.limit.unwrap_or(100);
    if limit <= 0 {
        return Err(bad_request(
            "invalid_limit",
            "limitは1以上で指定してください",
        ));
    }

    let summary = execute_fetch_content(&state.pool, limit, &state.scraping_api_url)
        .await
        .map_err(internal_error)?;

    if let Err(e) =
        webhook::notify_fetch_content(state.webhook_url.as_deref(), &summary, "api").await
    {
        warn!(error = %e, "Webhook送信に失敗しました(fetch-content)");
    }

    Ok(Json(summary))
}

#[derive(Debug, Deserialize)]
struct ArticleListQuery {
    limit: Option<i64>,
    page_token: Option<uuid::Uuid>,
}

#[derive(Debug, Serialize)]
struct ArticleItemResponse {
    id: uuid::Uuid,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    link: String,
    title: String,
    pub_date: Option<chrono::DateTime<chrono::Utc>>,
    description: String,
    group: Option<String>,
    content_brotli_base64: String,
}

#[derive(Debug, Serialize)]
struct ArticleListResponse {
    items: Vec<ArticleItemResponse>,
    next_token: Option<uuid::Uuid>,
}

async fn list_articles_handler(
    State(state): State<ApiState>,
    Query(params): Query<ArticleListQuery>,
) -> ApiResult<Json<ArticleListResponse>> {
    let limit_param = match params.limit {
        Some(value) if value <= 0 => {
            return Err(bad_request(
                "invalid_limit",
                "limitは1以上で指定してください",
            ));
        }
        Some(value) => value.min(MAX_LIMIT),
        None => UNSPECIFIED_LIMIT,
    };

    let cursor = if let Some(token) = params.page_token {
        match find_article_cursor(&state.pool, token).await {
            Ok(Some(cursor)) => Some(cursor),
            Ok(None) => {
                return Err(bad_request(
                    "page_token_not_found",
                    "page_token is not exist",
                ))
            }
            Err(e) => return Err(internal_error(e)),
        }
    } else {
        None
    };

    let fetch_limit = limit_param.checked_add(1).unwrap_or_else(|| limit_param);

    let articles = search_articles_window(&state.pool, fetch_limit, cursor.as_ref())
        .await
        .map_err(internal_error)?;

    let mut has_more = false;
    let mut trimmed_articles = articles;
    if trimmed_articles.len() as i64 == fetch_limit {
        has_more = true;
        trimmed_articles.truncate(limit_param as usize);
    }

    let mut total_base64_bytes = 0usize;
    let mut response_items = Vec::new();

    for article in &trimmed_articles {
        let encoded = STANDARD.encode(&article.data);
        if encoded.len() > MAX_RESPONSE_BYTES {
            return Err(error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "article_too_large",
                format!("記事ID {} の本文が応答許容量を超過しました", article.id),
            ));
        }

        if total_base64_bytes + encoded.len() > MAX_RESPONSE_BYTES {
            has_more = true;
            break;
        }

        total_base64_bytes += encoded.len();
        response_items.push(ArticleItemResponse {
            id: article.id,
            created_at: article.created_at,
            updated_at: article.updated_at,
            link: article.link.clone(),
            title: article.title.clone(),
            pub_date: article.pub_date,
            description: article.description.clone(),
            group: article.group.clone(),
            content_brotli_base64: encoded,
        });
    }

    if response_items.len() < trimmed_articles.len() {
        has_more = true;
    }

    let next_token = if has_more {
        response_items.last().map(|item| item.id)
    } else {
        None
    };

    Ok(Json(ArticleListResponse {
        items: response_items,
        next_token,
    }))
}

#[cfg(test)]
mod tests {
    pub mod fetch_rss_endpoint {
        use anyhow::Result;
        use axum::body::{to_bytes, Body};
        use axum::http::{Request, StatusCode};
        use serde_json::Value;
        use tower::ServiceExt;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        use crate::api::{build_router, ApiState};
        use crate::test_support::{clear_rss_tables, create_temp_yaml, prepare_test_pool};

        /// # 検証目的
        /// API経由でRSS取得処理が実行され、結果がJSONで返ることを確認する。
        #[tokio::test]
        async fn rssを取得するエンドポイントが動作する() -> Result<()> {
            let _lock = crate::test_support::acquire_db_lock().await;
            let pool = prepare_test_pool().await?;

            sqlx::migrate!("./migrations").run(&pool).await?;
            clear_rss_tables(&pool).await?;

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

            let temp_file =
                create_temp_yaml(&format!("test:\n  sample: {url}/feed", url = server.uri()))?;

            let state = ApiState::new(
                pool.clone(),
                server.uri(),
                temp_file.path().to_string_lossy().to_string(),
                None,
            );
            let app = build_router(state);

            let response = app
                .oneshot(Request::post("/api/fetch-rss").body(Body::empty()).unwrap())
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);

            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let value: Value = serde_json::from_slice(&bytes)?;
            assert_eq!(value["total_processed"].as_u64(), Some(1));

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
        use tower::ServiceExt;
        use uuid::Uuid;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        use crate::api::{build_router, ApiState};
        use crate::models::ArticleContent;
        use crate::test_support::{clear_rss_tables, prepare_test_pool};

        /// # 検証目的
        /// API経由でコンテンツ取得処理を実行し、本文保存とレスポンス内容を確認する。
        #[tokio::test]
        async fn コンテンツ取得apiが成功する() -> Result<()> {
            let _lock = crate::test_support::acquire_db_lock().await;
            let pool = prepare_test_pool().await?;

            sqlx::migrate!("./migrations").run(&pool).await?;
            clear_rss_tables(&pool).await?;

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

            let state = ApiState::new(
                pool.clone(),
                server.uri(),
                "rss_links.yml".to_string(),
                None,
            );
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

        /// # 検証目的
        /// limitに0を指定したリクエストが400エラーと`invalid_limit`コードを返すことを確認する。
        #[tokio::test]
        async fn limitが0ならエラーを返す() -> Result<()> {
            let _lock = crate::test_support::acquire_db_lock().await;
            let pool = prepare_test_pool().await?;

            sqlx::migrate!("./migrations").run(&pool).await?;
            clear_rss_tables(&pool).await?;

            let state = ApiState::new(
                pool,
                "http://localhost:8000".to_string(),
                "rss_links.yml".to_string(),
                None,
            );
            let app = build_router(state);

            let response = app
                .oneshot(
                    Request::post("/api/fetch-content")
                        .header("content-type", "application/json")
                        .body(Body::from("{\"limit\":0}"))
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes)?;
            assert_eq!(body["code"].as_str(), Some("invalid_limit"));
            assert_eq!(
                body["message"].as_str(),
                Some("limitは1以上で指定してください")
            );

            Ok(())
        }
    }

    pub mod pipeline_flow {
        use std::io::{Cursor, Read};

        use anyhow::Result;
        use brotli::Decompressor;
        use serde_json::json;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        use crate::articles::search_articles_window;
        use crate::fetch_content::execute_fetch_content;
        use crate::fetch_rss::execute_fetch_rss;
        use crate::test_support::{clear_rss_tables, create_temp_yaml, prepare_test_pool};

        /// # 検証目的
        /// RSS取り込みから本文保存、記事取得まで一連の流れが動作することを確認する。
        #[tokio::test]
        async fn rssから記事取得まで連携する() -> Result<()> {
            let _lock = crate::test_support::acquire_db_lock().await;
            let pool = prepare_test_pool().await?;

            sqlx::migrate!("./migrations").run(&pool).await?;
            clear_rss_tables(&pool).await?;

            let server = MockServer::start().await;

            let rss_body = r#"<?xml version="1.0" encoding="UTF-8"?>
                <rss version="2.0">
                  <channel>
                    <title>Integration Feed</title>
                    <item>
                      <title>Item Title</title>
                      <link>https://example.com/item</link>
                      <description>本文サマリ</description>
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

            let html_body = "<html><body>integration ok</body></html>";

            Mock::given(method("POST"))
                .and(path("/fetch"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "html": html_body,
                    "status_code": 200,
                    "title": "Integration",
                    "final_url": "https://example.com/item",
                    "elapsed_ms": 12.3,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                })))
                .mount(&server)
                .await;

            let temp_file = create_temp_yaml(&format!(
                "integration:\n  sample: {url}/feed",
                url = server.uri()
            ))?;

            let rss_summary =
                execute_fetch_rss(&pool, temp_file.path().to_string_lossy().as_ref()).await?;
            assert_eq!(rss_summary.total_processed, 1);

            let fetch_summary = execute_fetch_content(&pool, 10, &server.uri()).await?;
            assert_eq!(fetch_summary.saved_count, 1);
            assert_eq!(fetch_summary.status_only_count, 0);

            let articles = search_articles_window(&pool, 10, None).await?;
            assert_eq!(articles.len(), 1);
            let article = &articles[0];
            assert_eq!(article.link, "https://example.com/item");
            assert_eq!(article.title, "Item Title");

            let mut decompressor = Decompressor::new(Cursor::new(article.data.clone()), 4096);
            let mut decompressed = String::new();
            decompressor.read_to_string(&mut decompressed)?;
            assert_eq!(decompressed, html_body);

            let status: Option<i32> =
                sqlx::query_scalar("SELECT status_code FROM rss.queue WHERE id = $1")
                    .bind(article.id)
                    .fetch_one(&pool)
                    .await?;
            assert_eq!(status, Some(200));

            Ok(())
        }
    }

    pub mod articles_endpoint {
        use anyhow::Result;
        use axum::body::{to_bytes, Body};
        use axum::http::{Request, StatusCode};
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        use chrono::{Duration, Utc};
        use serde_json::Value;
        use sqlx::PgPool;
        use tower::ServiceExt;
        use uuid::Uuid;

        use crate::api::{build_router, ApiState};
        use crate::test_support::{clear_rss_tables, prepare_test_pool};

        async fn insert_article(
            pool: &PgPool,
            id: Uuid,
            created_at: chrono::DateTime<chrono::Utc>,
            link: &str,
            title: &str,
            description: &str,
            data: &[u8],
        ) -> Result<()> {
            sqlx::query(
                r#"
                INSERT INTO rss.queue (id, link, title, description, pub_date)
                VALUES ($1, $2, $3, $4, NOW())
                "#,
            )
            .bind(id)
            .bind(link)
            .bind(title)
            .bind(description)
            .execute(pool)
            .await?;

            sqlx::query(
                r#"
                UPDATE rss.queue
                SET created_at = $2, updated_at = $2
                WHERE id = $1
                "#,
            )
            .bind(id)
            .bind(created_at)
            .execute(pool)
            .await?;

            sqlx::query(
                r#"
                INSERT INTO rss.article_content (queue_id, data)
                VALUES ($1, $2)
                "#,
            )
            .bind(id)
            .bind(data)
            .execute(pool)
            .await?;

            Ok(())
        }

        /// # 検証目的
        /// 記事一覧APIがページネーションおよびBase64エンコードを正しく行うことを確認する。
        #[tokio::test]
        async fn 記事一覧を取得できる() -> Result<()> {
            let _lock = crate::test_support::acquire_db_lock().await;
            let pool = prepare_test_pool().await?;

            sqlx::migrate!("./migrations").run(&pool).await?;
            clear_rss_tables(&pool).await?;

            let newer_id = Uuid::new_v4();
            let older_id = Uuid::new_v4();

            insert_article(
                &pool,
                newer_id,
                Utc::now(),
                "https://example.com/new",
                "新しい記事",
                "新しい本文",
                b"newer",
            )
            .await?;

            insert_article(
                &pool,
                older_id,
                Utc::now() - Duration::hours(1),
                "https://example.com/old",
                "古い記事",
                "古い本文",
                b"older",
            )
            .await?;

            let state = ApiState::new(
                pool.clone(),
                "http://localhost:8000".to_string(),
                "rss_links.yml".to_string(),
                None,
            );
            let app = build_router(state);

            let response = app
                .clone()
                .oneshot(
                    Request::get("/api/articles?limit=1")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);

            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let body: Value = serde_json::from_slice(&bytes)?;

            let items = body["items"].as_array().expect("itemsが配列");
            assert_eq!(items.len(), 1);
            let first = &items[0];
            let first_id = first["id"].as_str().expect("idが文字列");
            assert_eq!(first_id, newer_id.to_string());

            let encoded = first["content_brotli_base64"]
                .as_str()
                .expect("content_brotli_base64が文字列");
            let decoded = STANDARD.decode(encoded)?;
            assert_eq!(decoded, b"newer");

            let next_token = body["next_token"].as_str().expect("next_tokenが存在");

            let response = app
                .oneshot(
                    Request::get(format!("/api/articles?page_token={}", next_token))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let body: Value = serde_json::from_slice(&bytes)?;
            let items = body["items"].as_array().expect("itemsが配列");
            assert_eq!(items.len(), 1);
            let first = &items[0];
            let first_id = first["id"].as_str().expect("idが文字列");
            assert_eq!(first_id, older_id.to_string());
            let encoded = first["content_brotli_base64"]
                .as_str()
                .expect("content_brotli_base64が文字列");
            let decoded = STANDARD.decode(encoded)?;
            assert_eq!(decoded, b"older");
            assert!(body["next_token"].is_null());

            Ok(())
        }

        /// # 検証目的
        /// 存在しないトークンを指定した場合にエラーが返ることを確認する。
        #[tokio::test]
        async fn 無効なトークンでエラーを返す() -> Result<()> {
            let _lock = crate::test_support::acquire_db_lock().await;
            let pool = prepare_test_pool().await?;

            sqlx::migrate!("./migrations").run(&pool).await?;
            clear_rss_tables(&pool).await?;

            let state = ApiState::new(
                pool,
                "http://localhost:8000".to_string(),
                "rss_links.yml".to_string(),
                None,
            );
            let app = build_router(state);

            let response = app
                .oneshot(
                    Request::get(format!("/api/articles?page_token={}", Uuid::new_v4()))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let body: Value = serde_json::from_slice(&bytes)?;
            assert_eq!(body["code"].as_str(), Some("page_token_not_found"));
            assert_eq!(body["message"].as_str(), Some("page_token is not exist"));

            Ok(())
        }

        /// # 検証目的
        /// limitに0を指定した場合に400エラーと`invalid_limit`コードが返ることを確認する。
        #[tokio::test]
        async fn 記事一覧のlimitが0ならエラー() -> Result<()> {
            let _lock = crate::test_support::acquire_db_lock().await;
            let pool = prepare_test_pool().await?;

            sqlx::migrate!("./migrations").run(&pool).await?;
            clear_rss_tables(&pool).await?;

            let state = ApiState::new(
                pool,
                "http://localhost:8000".to_string(),
                "rss_links.yml".to_string(),
                None,
            );
            let app = build_router(state);

            let response = app
                .oneshot(
                    Request::get("/api/articles?limit=0")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let body: Value = serde_json::from_slice(&bytes)?;
            assert_eq!(body["code"].as_str(), Some("invalid_limit"));
            assert_eq!(
                body["message"].as_str(),
                Some("limitは1以上で指定してください")
            );

            Ok(())
        }

        /// # 検証目的
        /// Base64化後に50MB制限を超える記事が含まれる場合に413エラーが返ることを確認する。
        #[tokio::test]
        async fn 応答サイズ超過時にエラー() -> Result<()> {
            let _lock = crate::test_support::acquire_db_lock().await;
            let pool = prepare_test_pool().await?;

            sqlx::migrate!("./migrations").run(&pool).await?;
            clear_rss_tables(&pool).await?;

            let large_data = vec![0u8; crate::api::MAX_RESPONSE_BYTES];
            let article_id = Uuid::new_v4();

            insert_article(
                &pool,
                article_id,
                Utc::now(),
                "https://example.com/huge",
                "巨大な記事",
                "巨大本文",
                &large_data,
            )
            .await?;

            let state = ApiState::new(
                pool,
                "http://localhost:8000".to_string(),
                "rss_links.yml".to_string(),
                None,
            );
            let app = build_router(state);

            let response = app
                .oneshot(Request::get("/api/articles").body(Body::empty()).unwrap())
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let body: Value = serde_json::from_slice(&bytes)?;
            assert_eq!(body["code"].as_str(), Some("article_too_large"));
            assert!(body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("記事ID"));

            Ok(())
        }
    }
}

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// queueテーブルのモデル
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Queue {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub link: String,
    pub title: String,
    pub pub_date: Option<DateTime<Utc>>,
    pub description: String,
    pub status_code: Option<i32>,
    pub group: Option<String>,
}

/// article_contentテーブルのモデル
#[derive(Debug, Clone, FromRow)]
pub struct ArticleContent {
    pub queue_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub data: Vec<u8>,
}

/// queueへの新規挿入用構造体
#[derive(Debug, Clone)]
pub struct NewQueue {
    pub link: String,
    pub title: String,
    pub pub_date: Option<DateTime<Utc>>,
    pub description: String,
    pub group: Option<String>,
}

/// RSSリンク設定（rss_links.ymlから読み込む）
#[derive(Debug, Clone, Deserialize)]
pub struct RssLinks {
    #[serde(flatten)]
    pub groups: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
}

/// スクレイピングAPIリクエスト
#[derive(Debug, Clone, Serialize)]
pub struct ScrapeRequest {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_for_selector: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
}

/// スクレイピングAPIレスポンス
#[derive(Debug, Clone, Deserialize)]
pub struct ScrapeResponse {
    pub html: String,
    pub status_code: i32,
    pub title: String,
    pub final_url: String,
    pub elapsed_ms: f64,
    pub timestamp: String,
}

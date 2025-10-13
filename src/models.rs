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
    groups: std::collections::HashMap<String, std::collections::HashMap<String, RssLinkEntry>>,
}

impl RssLinks {
    /// フラット化したフィード一覧を取得
    pub fn into_sources(self) -> Vec<RssFeedSource> {
        let mut feeds = Vec::new();

        for (group, entries) in self.groups {
            for (name, entry) in entries {
                let url = match entry {
                    RssLinkEntry::Url(url) => url,
                    RssLinkEntry::Detailed { url, .. } => url,
                };

                feeds.push(RssFeedSource {
                    group: group.clone(),
                    name,
                    url,
                });
            }
        }

        feeds
    }
}

#[derive(Debug, Clone)]
pub struct RssFeedSource {
    pub group: String,
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum RssLinkEntry {
    Url(String),
    Detailed {
        url: String,
        #[serde(default)]
        _wait_for_selector: Option<String>,
        #[serde(default)]
        _timeout: Option<u64>,
    },
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
}

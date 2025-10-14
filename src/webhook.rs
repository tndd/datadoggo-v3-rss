use std::time::Duration;

use anyhow::Result;
use once_cell::sync::Lazy;
use reqwest::Client;
use serde::Serialize;
use serde_json::json;

use crate::fetch_content::FetchContentSummary;
use crate::fetch_rss::FetchRssSummary;

/// Webhook POSTのタイムアウト秒数
pub(crate) const WEBHOOK_TIMEOUT_SECS: u64 = 5;

static WEBHOOK_CLIENT: Lazy<Client> = Lazy::new(|| {
    Client::builder()
        .timeout(Duration::from_secs(WEBHOOK_TIMEOUT_SECS))
        .build()
        .expect("Webhook用Clientの初期化に失敗")
});

/// Webhookへ通知を送る。URLが未設定の場合は何もしない。
pub async fn notify_fetch_rss(
    webhook_url: Option<&str>,
    summary: &FetchRssSummary,
    source: &str,
) -> Result<()> {
    if let Some(url) = webhook_url {
        let payload = json!({
            "event": "fetch_rss",
            "source": source,
            "summary": summary,
        });
        send(url, &payload).await?;
    }
    Ok(())
}

/// Webhookへfetch-contentの結果を通知する。
pub async fn notify_fetch_content(
    webhook_url: Option<&str>,
    summary: &FetchContentSummary,
    source: &str,
) -> Result<()> {
    if let Some(url) = webhook_url {
        let payload = json!({
            "event": "fetch_content",
            "source": source,
            "summary": summary,
        });
        send(url, &payload).await?;
    }
    Ok(())
}

async fn send<T: Serialize>(url: &str, payload: &T) -> Result<()> {
    WEBHOOK_CLIENT
        .post(url)
        .json(payload)
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    pub mod webhook_notify {
        use std::time::Duration;

        use anyhow::Result;
        use serde_json::json;
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        use crate::fetch_content::FetchContentSummary;
        use crate::fetch_rss::{FetchRssFeedResult, FetchRssSummary};
        use crate::webhook::{notify_fetch_content, notify_fetch_rss, WEBHOOK_TIMEOUT_SECS};

        /// # 検証目的
        /// fetch-rssのサマリがWebhookへPOSTされることを確認する。
        #[tokio::test]
        async fn fetch_rssの通知を送信できる() -> Result<()> {
            let server = MockServer::start().await;

            let expected = json!({
                "event": "fetch_rss",
                "source": "test",
                "summary": {
                    "total_processed": 1,
                    "feeds": [
                        {
                            "group": "test",
                            "name": "feed",
                            "processed": 1,
                            "error": null
                        }
                    ]
                }
            });

            Mock::given(method("POST"))
                .and(path("/hook"))
                .and(body_json(&expected))
                .respond_with(ResponseTemplate::new(200))
                .mount(&server)
                .await;

            let summary = FetchRssSummary {
                total_processed: 1,
                feeds: vec![FetchRssFeedResult {
                    group: "test".to_string(),
                    name: "feed".to_string(),
                    processed: 1,
                    error: None,
                }],
            };

            notify_fetch_rss(Some(&format!("{}/hook", server.uri())), &summary, "test").await?;

            Ok(())
        }

        /// # 検証目的
        /// fetch-contentのサマリがWebhookへPOSTされることを確認する。
        #[tokio::test]
        async fn fetch_contentの通知を送信できる() -> Result<()> {
            let server = MockServer::start().await;

            let summary = FetchContentSummary {
                saved_count: 1,
                status_only_count: 0,
                error_count: 0,
                entries: Vec::new(),
            };

            let expected = json!({
                "event": "fetch_content",
                "source": "test",
                "summary": {
                    "saved_count": 1,
                    "status_only_count": 0,
                    "error_count": 0,
                    "entries": []
                },
            });

            Mock::given(method("POST"))
                .and(path("/hook"))
                .and(body_json(&expected))
                .respond_with(ResponseTemplate::new(200))
                .mount(&server)
                .await;

            notify_fetch_content(Some(&format!("{}/hook", server.uri())), &summary, "test").await?;

            Ok(())
        }

        /// # 検証目的
        /// Webhookが応答しない場合でも、設定したタイムアウトで早期に制御が戻ることを確認する。
        #[tokio::test]
        async fn 応答が無い場合はタイムアウトする() -> Result<()> {
            let server = MockServer::start().await;

            Mock::given(method("POST"))
                .and(path("/slow"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_delay(Duration::from_secs(WEBHOOK_TIMEOUT_SECS + 5)),
                )
                .mount(&server)
                .await;

            let summary = FetchRssSummary {
                total_processed: 0,
                feeds: Vec::new(),
            };

            let before = tokio::time::Instant::now();
            let result =
                notify_fetch_rss(Some(&format!("{}/slow", server.uri())), &summary, "test").await;
            let elapsed = before.elapsed();

            assert!(result.is_err(), "タイムアウトエラーを期待");
            assert!(
                elapsed < Duration::from_secs(WEBHOOK_TIMEOUT_SECS + 3),
                "タイムアウトまでに想定以上の時間がかかっています: {:?}",
                elapsed
            );

            Ok(())
        }
    }
}

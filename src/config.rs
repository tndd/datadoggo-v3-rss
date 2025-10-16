use anyhow::Result;
use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub scraping_api_url: String,
    pub webhook_url: Option<String>,
}

impl Config {
    /// 環境変数から設定を読み込む
    pub fn from_env() -> Result<Self> {
        dotenv::dotenv().ok();

        let database_url = Self::get_database_url()?;

        let scraping_api_url =
            env::var("SCRAPING_API_URL").unwrap_or_else(|_| "http://localhost:8000".to_string());

        let webhook_url = env::var("WEBHOOK_URL").ok();

        Ok(Config {
            database_url,
            scraping_api_url,
            webhook_url,
        })
    }

    /// データベースURLを取得する
    ///
    /// 優先順位:
    /// 1. DATABASE_URL環境変数が直接指定されている場合はそれを使用
    /// 2. ENVIRONMENT環境変数に基づいて適切なURLを選択
    ///    - TEST: DATABASE_URL_TEST (デフォルト)
    ///    - STG: DATABASE_URL_STG
    ///    - PROD: DATABASE_URL_PROD (PROD_CONFIRMED=trueが必要)
    fn get_database_url() -> Result<String> {
        // DATABASE_URLが直接指定されている場合は最優先
        if let Ok(url) = env::var("DATABASE_URL") {
            return Ok(url);
        }

        // ENVIRONMENTに基づいて適切なURLを選択（デフォルトはTEST）
        let environment = env::var("ENVIRONMENT").unwrap_or_else(|_| "TEST".to_string());

        let url = match environment.to_uppercase().as_str() {
            "TEST" => env::var("DATABASE_URL_TEST")
                .map_err(|_| anyhow::anyhow!("DATABASE_URL_TESTが設定されていません"))?,
            "STG" => env::var("DATABASE_URL_STG")
                .map_err(|_| anyhow::anyhow!("DATABASE_URL_STGが設定されていません"))?,
            "PROD" => {
                // 本番環境への安全装置
                let confirmed = env::var("PROD_CONFIRMED").unwrap_or_else(|_| "false".to_string());
                if confirmed.to_lowercase() != "true" {
                    return Err(anyhow::anyhow!(
                        "本番環境に接続するにはPROD_CONFIRMED=trueを設定してください"
                    ));
                }
                env::var("DATABASE_URL_PROD")
                    .map_err(|_| anyhow::anyhow!("DATABASE_URL_PRODが設定されていません"))?
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "無効なENVIRONMENT: {} (有効な値: TEST, STG, PROD)",
                    environment
                ))
            }
        };

        Ok(url)
    }
}

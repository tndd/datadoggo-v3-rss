use anyhow::Result;
use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub scraping_api_url: String,
}

impl Config {
    /// 環境変数から設定を読み込む
    pub fn from_env() -> Result<Self> {
        dotenv::dotenv().ok();

        let database_url = env::var("DATABASE_URL")
            .map_err(|_| anyhow::anyhow!("DATABASE_URLが設定されていません"))?;

        let scraping_api_url =
            env::var("SCRAPING_API_URL").unwrap_or_else(|_| "http://localhost:8000".to_string());

        Ok(Config {
            database_url,
            scraping_api_url,
        })
    }
}

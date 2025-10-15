mod api;
mod articles;
mod config;
mod db;
mod fetch_content;
mod fetch_rss;
mod models;
mod webhook;

#[cfg(test)]
mod test_support;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::net::IpAddr;
use tracing::info;

fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

#[derive(Parser)]
#[command(name = "datadoggo-v3-rss")]
#[command(about = "RSSフィードから記事を収集してDBに保存する", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// RSSフィードから新規記事をqueueに登録
    FetchRss,

    /// queue内のstatus_code=NULLな記事に対してAPI実行
    FetchContent {
        /// 処理する最大件数（デフォルト: 100）
        #[arg(short, long, default_value = "100")]
        limit: i64,
    },

    /// APIサーバを起動
    Serve {
        /// バインドするホスト（デフォルト: 127.0.0.1）
        #[arg(long, default_value = "127.0.0.1")]
        host: IpAddr,

        /// バインドするポート（デフォルト: 8080）
        #[arg(long, default_value_t = 8080)]
        port: u16,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let cli = Cli::parse();

    // 設定を読み込む
    let config = config::Config::from_env()?;

    // データベース接続プールを作成
    let pool = db::create_pool(&config.database_url).await?;

    match cli.command {
        Commands::FetchRss => {
            info!("=== fetch-rss コマンドを実行 ===");
            fetch_rss::run(pool, config.webhook_url.as_deref()).await?;
        }
        Commands::FetchContent { limit } => {
            info!("=== fetch-content コマンドを実行 ===");
            fetch_content::run(
                pool,
                limit,
                &config.scraping_api_url,
                config.webhook_url.as_deref(),
            )
            .await?;
        }
        Commands::Serve { host, port } => {
            info!("=== APIサーバを起動 ===");
            let state = api::ApiState::new(
                pool.clone(),
                config.scraping_api_url.clone(),
                "rss_links.yml".to_string(),
                config.webhook_url.clone(),
            );
            api::serve(state, host, port).await?;
        }
    }

    Ok(())
}

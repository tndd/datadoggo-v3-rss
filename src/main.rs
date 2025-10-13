mod config;
mod db;
mod fetch_content;
mod fetch_rss;
mod models;

#[cfg(test)]
mod test_support;

use anyhow::Result;
use clap::{Parser, Subcommand};

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
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // 設定を読み込む
    let config = config::Config::from_env()?;

    // データベース接続プールを作成
    let pool = db::create_pool(&config.database_url).await?;

    match cli.command {
        Commands::FetchRss => {
            println!("=== fetch-rss コマンドを実行 ===\n");
            fetch_rss::run(pool).await?;
        }
        Commands::FetchContent { limit } => {
            println!("=== fetch-content コマンドを実行 ===\n");
            fetch_content::run(pool, limit, &config.scraping_api_url).await?;
        }
    }

    Ok(())
}

use anyhow::Result;
use sqlx::{postgres::PgPoolOptions, PgPool};

/// データベース接続プールを作成
pub async fn create_pool(database_url: &str) -> Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await?;

    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_create_pool_with_test_database() {
        // TEST_DATABASE_URLが未設定の場合はスキップ
        let db_url = match std::env::var("TEST_DATABASE_URL") {
            Ok(url) => url,
            Err(_) => {
                eprintln!("TEST_DATABASE_URLが未設定のためスキップ");
                return;
            }
        };

        let result = create_pool(&db_url).await;
        assert!(result.is_ok(), "テスト用データベースへの接続に失敗");
    }
}

use anyhow::Result;
use sqlx::{PgPool, postgres::PgPoolOptions};

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
    async fn test_create_pool() {
        // 環境変数が正しく設定されている場合のみテストを実行
        if let Ok(db_url) = std::env::var("DATABASE_URL") {
            let result = create_pool(&db_url).await;
            assert!(result.is_ok(), "データベース接続プールの作成に失敗");
        }
    }
}

use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

/// queueとarticle_contentを結合した記事データ
#[allow(dead_code)] // 将来のAPI向けに用意しており現時点では内部から参照されない
#[derive(Debug, Clone, FromRow)]
pub struct Article {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub link: String,
    pub title: String,
    pub pub_date: Option<DateTime<Utc>>,
    pub description: String,
    pub data: Vec<u8>,
    pub group: Option<String>,
}

/// 最新の記事を取得する。limit件数分のみ返す。
#[allow(dead_code)] // 将来のAPI向けに用意しており現時点では内部から参照されない
pub async fn search_articles(pool: &PgPool, limit: i64) -> Result<Vec<Article>> {
    let articles = sqlx::query_as::<_, Article>(
        r#"
        SELECT
            q.id,
            q.created_at,
            q.updated_at,
            q.link,
            q.title,
            q.pub_date,
            q.description,
            ac.data,
            q."group"
        FROM rss.queue AS q
        INNER JOIN rss.article_content AS ac ON ac.queue_id = q.id
        ORDER BY q.created_at DESC
        LIMIT $1
        "#,
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(articles)
}

/// ページネーション用カーソル
#[derive(Debug, Clone)]
pub struct ArticleCursor {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
}

/// 指定したIDのカーソル情報を取得する
pub async fn find_article_cursor(pool: &PgPool, id: Uuid) -> Result<Option<ArticleCursor>> {
    let row = sqlx::query_as::<_, (DateTime<Utc>,)>(
        r#"
        SELECT created_at
        FROM rss.queue
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|record| ArticleCursor {
        id,
        created_at: record.0,
    }))
}

/// ページネーション条件に従い記事を検索する。limitに+αした件数を取得し、呼び出し側で件数調整する想定。
pub async fn search_articles_window(
    pool: &PgPool,
    limit: i64,
    cursor: Option<&ArticleCursor>,
) -> Result<Vec<Article>> {
    let articles = sqlx::query_as::<_, Article>(
        r#"
        SELECT
            q.id,
            q.created_at,
            q.updated_at,
            q.link,
            q.title,
            q.pub_date,
            q.description,
            ac.data,
            q."group"
        FROM rss.queue AS q
        INNER JOIN rss.article_content AS ac ON ac.queue_id = q.id
        WHERE (
            $2::timestamptz IS NULL
            OR q.created_at < $2
            OR (q.created_at = $2 AND q.id < $3)
        )
        ORDER BY q.created_at DESC, q.id DESC
        LIMIT $1
        "#,
    )
    .bind(limit)
    .bind(cursor.map(|c| c.created_at))
    .bind(cursor.map(|c| c.id))
    .fetch_all(pool)
    .await?;

    Ok(articles)
}

#[cfg(test)]
mod tests {
    pub mod search_articles {
        use anyhow::Result;
        use uuid::Uuid;

        use crate::articles::search_articles;
        use crate::test_support::{clear_rss_tables, fixed_datetime, prepare_test_pool};

        /// # 検証目的
        /// queueとarticle_contentを結合した記事が取得でき、limit件数が機能することを確認する。
        #[tokio::test]
        async fn 記事を取得できる() -> Result<()> {
            let _lock = crate::test_support::acquire_db_lock().await;
            let pool = prepare_test_pool().await?;

            sqlx::migrate!("./migrations").run(&pool).await?;
            clear_rss_tables(&pool).await?;

            let first_id = Uuid::new_v4();
            let second_id = Uuid::new_v4();
            let skip_id = Uuid::new_v4();

            sqlx::query(
                r#"
                INSERT INTO rss.queue (id, link, title, description, pub_date, "group")
                VALUES ($1, $2, $3, $4, $5, $6)
                "#,
            )
            .bind(first_id)
            .bind("https://example.com/first")
            .bind("最初の記事")
            .bind("本文1")
            .bind(Some(fixed_datetime(2025, 10, 12, 12, 0, 0)))
            .bind(Some("world"))
            .execute(&pool)
            .await?;

            sqlx::query(
                r#"
                INSERT INTO rss.queue (id, link, title, description, pub_date)
                VALUES ($1, $2, $3, $4, $5)
                "#,
            )
            .bind(second_id)
            .bind("https://example.com/second")
            .bind("二番目の記事")
            .bind("本文2")
            .bind(Some(fixed_datetime(2025, 10, 12, 13, 0, 0)))
            .execute(&pool)
            .await?;

            sqlx::query(
                r#"
                INSERT INTO rss.queue (id, link, title, description)
                VALUES ($1, $2, $3, $4)
                "#,
            )
            .bind(skip_id)
            .bind("https://example.com/skip")
            .bind("未取得の記事")
            .bind("本文なし")
            .execute(&pool)
            .await?;

            sqlx::query(
                r#"
                INSERT INTO rss.article_content (queue_id, data)
                VALUES ($1, $2)
                "#,
            )
            .bind(first_id)
            .bind(b"first".to_vec())
            .execute(&pool)
            .await?;

            sqlx::query(
                r#"
                INSERT INTO rss.article_content (queue_id, data)
                VALUES ($1, $2)
                "#,
            )
            .bind(second_id)
            .bind(b"second".to_vec())
            .execute(&pool)
            .await?;

            let articles = search_articles(&pool, 1).await?;
            assert_eq!(articles.len(), 1);
            assert_eq!(articles[0].id, second_id);
            assert_eq!(articles[0].data, b"second".to_vec());

            let articles = search_articles(&pool, 10).await?;
            assert_eq!(articles.len(), 2);
            assert!(articles.iter().all(|a| a.id != skip_id));

            Ok(())
        }
    }
}

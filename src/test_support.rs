use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, TimeZone, Utc};
use once_cell::sync::Lazy;
use sqlx::PgPool;
use tokio::sync::{Mutex, MutexGuard};
use uuid::Uuid;

static DB_MUTEX: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

/// テスト用DB操作の同時実行を防止
pub async fn acquire_db_lock() -> MutexGuard<'static, ()> {
    DB_MUTEX.lock().await
}

/// TEST_DATABASE_URLをもとに接続プールを準備する。未設定の場合はNoneを返す。
pub async fn prepare_test_pool() -> Option<PgPool> {
    match std::env::var("TEST_DATABASE_URL") {
        Ok(url) => match PgPool::connect(&url).await {
            Ok(pool) => Some(pool),
            Err(e) => {
                eprintln!("TEST_DATABASE_URLへ接続できないためスキップ: {}", e);
                None
            }
        },
        Err(_) => {
            eprintln!("TEST_DATABASE_URLが設定されていないためスキップ");
            None
        }
    }
}

/// RSS関連テーブルを初期化する。
pub async fn clear_rss_tables(pool: &PgPool) -> Result<()> {
    sqlx::query("TRUNCATE rss.article_content CASCADE")
        .execute(pool)
        .await?;
    sqlx::query("TRUNCATE rss.queue CASCADE")
        .execute(pool)
        .await?;
    Ok(())
}

/// queueのcreated_at/updated_atを固定値へ更新する。
pub async fn set_queue_timestamp(pool: &PgPool, id: Uuid, timestamp: DateTime<Utc>) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE rss.queue
        SET created_at = $2, updated_at = $2
        WHERE id = $1
        "#,
    )
    .bind(id)
    .bind(timestamp)
    .execute(pool)
    .await?;

    Ok(())
}

/// 日付時刻を安全に生成する。
pub fn fixed_datetime(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, hour, minute, second)
        .single()
        .expect("有効な日時を生成できなかった")
}

/// 一時的なYAMLを管理する。Drop時に自動削除する。
pub struct TempYamlFile {
    path: PathBuf,
}

impl TempYamlFile {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempYamlFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// 一時的なYAMLファイルを生成し、ガードを返す。
pub fn create_temp_yaml(content: &str) -> Result<TempYamlFile> {
    let path = std::env::temp_dir().join(format!("rss_links_{}.yml", Uuid::new_v4()));
    let mut file = fs::File::create(&path)?;
    file.write_all(content.as_bytes())?;
    file.flush()?;
    Ok(TempYamlFile { path })
}

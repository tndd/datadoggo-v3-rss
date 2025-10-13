use once_cell::sync::Lazy;
use tokio::sync::{Mutex, MutexGuard};

static DB_MUTEX: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

/// テスト用DB操作の同時実行を防止
pub async fn acquire_db_lock() -> MutexGuard<'static, ()> {
    DB_MUTEX.lock().await
}

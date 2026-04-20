//! # SQLite-backed [`ClientStorage`] implementation
//!
//! File-backed persistence using `rusqlite`. One table per
//! [`crate::client::client_storage::client_storage::BUCKETS`] entry, each with
//! `(key TEXT PRIMARY KEY, value BLOB, last_accessed INTEGER)`. Opened with WAL mode
//! (concurrent readers while a writer commits) and `PRAGMA synchronous = NORMAL` — safe
//! for local cache use, not for power-loss durability.
//!
//! A `parking_lot::Mutex` serialises all SQLite access so we never have two goroutines
//! hitting the same connection at once. This is the store the native server binary and
//! the desktop reference client use.

use crate::client::client_storage::client_storage::{ClientStorage, BUCKETS};
use crate::tools::time::TimeMillis;
use anyhow::Context;
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::Arc;
use parking_lot::Mutex;

pub struct SqliteClientStorage {
    connection: Arc<Mutex<Connection>>,
}

impl SqliteClientStorage {
    /// Creates a new SqliteClientStorage at `{data_dir}/client_storage.db`.
    /// The `data_dir` directory must already exist.
    pub async fn new(data_dir: PathBuf) -> anyhow::Result<Arc<Self>> {
        let database_path = data_dir.join("client_storage.db");
        let connection = Connection::open(&database_path)
            .with_context(|| format!("Failed to open SQLite database at {}", database_path.display()))?;

        // WAL mode allows concurrent readers and writers without blocking.
        // NORMAL sync reduces fsync calls for better write performance — safe against
        // process crashes but not OS/power crashes (acceptable for a local cache).
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;

        for bucket in BUCKETS {
            let table_name = sanitize_bucket_name(bucket);
            connection.execute_batch(&format!(
                "CREATE TABLE IF NOT EXISTS [{table_name}] (
                    key TEXT PRIMARY KEY,
                    value BLOB NOT NULL,
                    last_accessed INTEGER NOT NULL
                )"
            ))?;
        }

        Ok(Arc::new(Self {
            connection: Arc::new(Mutex::new(connection)),
        }))
    }
}

fn sanitize_bucket_name(bucket: &str) -> String {
    bucket.replace(|c: char| !c.is_alphanumeric() && c != '_', "_")
}

#[async_trait::async_trait]
impl ClientStorage for SqliteClientStorage {
    async fn count(&self, bucket: &str) -> anyhow::Result<usize> {
        let table_name = sanitize_bucket_name(bucket);
        let connection = self.connection.lock();
        let count: usize = connection.query_row(
            &format!("SELECT COUNT(*) FROM [{table_name}]"),
            [],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    async fn keys(&self, bucket: &str) -> anyhow::Result<Vec<String>> {
        let table_name = sanitize_bucket_name(bucket);
        let connection = self.connection.lock();
        let mut statement = connection.prepare(&format!("SELECT key FROM [{table_name}]"))?;
        let keys = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(keys)
    }

    async fn get(&self, bucket: &str, key: &str, time_millis: TimeMillis) -> anyhow::Result<Option<Vec<u8>>> {
        let table_name = sanitize_bucket_name(bucket);
        let connection = self.connection.lock();

        let result: Option<Vec<u8>> = connection
            .query_row(
                &format!("SELECT value FROM [{table_name}] WHERE key = ?1"),
                [key],
                |row| row.get(0),
            )
            .optional()?;

        if result.is_some() && time_millis > TimeMillis::zero() {
            connection.execute(
                &format!("UPDATE [{table_name}] SET last_accessed = ?1 WHERE key = ?2"),
                rusqlite::params![time_millis.0, key],
            )?;
        }

        Ok(result)
    }

    async fn put(&self, bucket: &str, key: &str, value: Vec<u8>, time_millis: TimeMillis) -> anyhow::Result<()> {
        let table_name = sanitize_bucket_name(bucket);
        let connection = self.connection.lock();
        connection.execute(
            &format!("INSERT OR REPLACE INTO [{table_name}] (key, value, last_accessed) VALUES (?1, ?2, ?3)"),
            rusqlite::params![key, value, time_millis.0],
        )?;
        Ok(())
    }

    async fn remove(&self, bucket: &str, key: &str) -> anyhow::Result<()> {
        let table_name = sanitize_bucket_name(bucket);
        let connection = self.connection.lock();
        connection.execute(
            &format!("DELETE FROM [{table_name}] WHERE key = ?1"),
            [key],
        )?;
        Ok(())
    }

    async fn trim(&self, bucket: &str, max_count: usize) -> anyhow::Result<()> {
        let table_name = sanitize_bucket_name(bucket);
        let connection = self.connection.lock();

        let count: usize = connection.query_row(
            &format!("SELECT COUNT(*) FROM [{table_name}]"),
            [],
            |row| row.get(0),
        )?;

        if count > max_count {
            let num_to_delete = count - max_count;
            connection.execute(
                &format!("DELETE FROM [{table_name}] WHERE key IN (SELECT key FROM [{table_name}] ORDER BY last_accessed ASC LIMIT ?1)"),
                [num_to_delete],
            )?;
        }

        Ok(())
    }

    async fn reset(&self) -> anyhow::Result<()> {
        let connection = self.connection.lock();
        for bucket in BUCKETS {
            let table_name = sanitize_bucket_name(bucket);
            connection.execute_batch(&format!("DELETE FROM [{table_name}]"))?;
        }
        Ok(())
    }
}

use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use crate::client::client_storage::client_storage;
    use crate::client::client_storage::sqlite_client_storage::SqliteClientStorage;
    use crate::tools::tools::get_temp_dir;

    #[tokio::test]
    async fn add_test() {
        let (_temp_dir, temp_dir_path) = get_temp_dir().unwrap();
        let storage = SqliteClientStorage::new(temp_dir_path.into()).await.unwrap();
        client_storage::tests::add_test(storage).await;
    }

    #[tokio::test]
    async fn trim_test() {
        let (_temp_dir, temp_dir_path) = get_temp_dir().unwrap();
        let storage = SqliteClientStorage::new(temp_dir_path.into()).await.unwrap();
        client_storage::tests::trim_test(storage).await;
    }
}

use crate::client::client_storage::client_storage::{ClientStorage, BUCKETS};
use crate::tools::time::TimeMillis;
use anyhow::anyhow;
use indexed_db_futures::database::Database;
use indexed_db_futures::error::Error;
use indexed_db_futures::prelude::*;
use indexed_db_futures::transaction::TransactionMode;
use indexed_db_futures::KeyPath;
use log::{info, warn};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const DATABASE_NAME: &str = "hashiverse.client_storage";

#[derive(Serialize, Deserialize)]
struct Record {
    key: String,
    value: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct RecordMetadata {
    key: String,
    last_accessed: i64,
    length: u32,
}

pub struct WasmClientStorage {}

fn build_table(db: &Database, name: &str) -> Result<(), Error> {
    info!("Building table {}", name);

    // Object
    {
        let _object_store = db.create_object_store(name).with_key_path(KeyPath::from("key")).build()?;
    }

    // Metadata
    {
        let name_metadata = format!("{}.metadata", name);

        let object_store = db.create_object_store(name_metadata).with_key_path(KeyPath::from("key")).build()?;

        object_store.create_index("last_accessed", KeyPath::from("last_accessed")).with_unique(false).with_multi_entry(false).build()?;
    }

    Ok(())
}

/// Opens (and if necessary creates/upgrades) the client storage IndexedDB.
///
/// Every call site goes through this so that the version and upgrade handler
/// are always applied — even if the database did not exist yet.
async fn open_database() -> anyhow::Result<Database> {
    let database = Database::open(DATABASE_NAME)
        .with_version(1u8)
        .with_on_blocked(|event| {
            warn!("indexed_db upgrade blocked: {:?}", event);
            Ok(())
        })
        .with_on_upgrade_needed(|event, db| {
            let old_version = event.old_version() as u64;
            let new_version = event.new_version().map(|v| v as u64);
            warn!("indexed_db upgrade needed from {:?} to {:?}", old_version, new_version);

            match (old_version, new_version) {
                (0, Some(1)) => {
                    for bucket in BUCKETS {
                        build_table(&db, bucket)?;
                    }
                }
                _ => {
                    warn!("Unhandled upgrade from indexed_db old={:?} to new={:?}", old_version, new_version);
                }
            }

            Ok(())
        })
        .build()
        .map_err(|e| anyhow!("{}", e))?
        .await
        .map_err(|e| anyhow!("{}", e))?;

    Ok(database)
}

impl WasmClientStorage {
    async fn get_database(&self) -> anyhow::Result<Database> {
        open_database().await
    }

    pub async fn new() -> anyhow::Result<Arc<Self>> {
        let _database = open_database().await?;
        Ok(Arc::new(Self {}))
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl ClientStorage for WasmClientStorage {
    async fn count(&self, bucket: &str) -> anyhow::Result<usize> {
        let database = self.get_database().await?;
        let result = try {
            let transaction = database.transaction(bucket).with_mode(TransactionMode::Readwrite).build()?;
            let object_store = transaction.object_store(bucket)?;
            object_store.count().await? as usize
        };

        match result {
            Ok(x) => Ok(x),
            Err(e) => Err(anyhow::anyhow!("{}", e)),
        }
    }

    async fn keys(&self, bucket: &str) -> anyhow::Result<Vec<String>> {
        let database = self.get_database().await?;
        let result = try {
            let transaction = database.transaction(bucket).with_mode(TransactionMode::Readonly).build()?;
            let object_store = transaction.object_store(bucket)?;
            let all_records: Vec<Record> = object_store.get_all().serde()?.await?.collect::<Result<Vec<_>, _>>()?;
            all_records.into_iter().map(|r| r.key).collect()
        };
        match result {
            Ok(x) => Ok(x),
            Err(e) => Err(anyhow::anyhow!("{}", e)),
        }
    }

    async fn get(&self, bucket: &str, key: &str, time_millis: TimeMillis) -> anyhow::Result<Option<Vec<u8>>> {
        let database = self.get_database().await?;
        let result = try {
            let bucket_metadata = format!("{}.metadata", bucket);
            let transaction = database.transaction([bucket, bucket_metadata.as_str()]).with_mode(TransactionMode::Readwrite).build()?;

            let value = {
                let object_store = transaction.object_store(bucket)?;
                let record: Option<Record> = object_store.get(key).serde()?.await?;

                match &record {
                    Some(record) => Some(record.value.clone()),
                    None => None,
                }
            };

            // Update the metadata only if we have been told to do so
            if time_millis > TimeMillis::zero() {
                if let Some(value) = &value {
                    let object_store_metadata = transaction.object_store(bucket_metadata.as_ref())?;
                    object_store_metadata
                        .put(RecordMetadata {
                            key: key.to_string(),
                            last_accessed: time_millis.0,
                            length: value.len() as u32,
                        })
                        .serde()?
                        .await?;
                }
            }

            transaction.commit().await?;

            value
        };

        match result {
            Ok(x) => Ok(x),
            Err(e) => Err(anyhow::anyhow!("{}", e)),
        }
    }

    async fn put(&self, bucket: &str, key: &str, value: Vec<u8>, time_millis: TimeMillis) -> anyhow::Result<()> {
        let database = self.get_database().await?;
        let result = try {
            let bucket_metadata = format!("{}.metadata", bucket);
            let transaction = database.transaction([bucket, bucket_metadata.as_ref()]).with_mode(TransactionMode::Readwrite).build()?;

            // Metadata
            {
                let object_store_metadata = transaction.object_store(bucket_metadata.as_ref())?;
                object_store_metadata
                    .put(RecordMetadata {
                        key: key.to_string(),
                        last_accessed: time_millis.0,
                        length: value.len() as u32,
                    })
                    .serde()?
                    .await?;
            }

            // Object
            {
                let object_store = transaction.object_store(bucket)?;
                object_store.put(Record { key: key.to_string(), value }).serde()?.await?;
            }

            transaction.commit().await?;
        };

        match result {
            Ok(x) => Ok(x),
            Err(e) => Err(anyhow::anyhow!("{}", e)),
        }
    }

    async fn remove(&self, bucket: &str, key: &str) -> anyhow::Result<()> {
        let database = self.get_database().await?;
        let result = try {
            let bucket_metadata = format!("{}.metadata", bucket);
            let transaction = database.transaction([bucket, bucket_metadata.as_ref()]).with_mode(TransactionMode::Readwrite).build()?;

            // Metadata
            {
                let object_store_metadata = transaction.object_store(bucket_metadata.as_ref())?;
                object_store_metadata.delete(key.to_string()).await?;
            }

            // Object
            {
                let object_store = transaction.object_store(bucket)?;
                object_store.delete(key.to_string()).await?;
            }

            transaction.commit().await?;
        };

        match result {
            Ok(x) => Ok(x),
            Err(e) => Err(anyhow::anyhow!("{}", e)),
        }
    }

    async fn trim(&self, bucket: &str, max_count: usize) -> anyhow::Result<()> {
        let database = self.get_database().await?;
        let result = try {
            let bucket_metadata = format!("{}.metadata", bucket);
            let transaction = database.transaction([bucket, bucket_metadata.as_str()]).with_mode(TransactionMode::Readwrite).build()?;

            let total = transaction.object_store(bucket_metadata.as_ref())?.count().await? as usize;
            if total > max_count {
                let num_to_delete = total - max_count;
                info!("Trimming {} records from {}", num_to_delete, bucket);

                // Cursor on the last_accessed index iterates oldest-first
                if let Some(mut cursor) = transaction.object_store(bucket_metadata.as_ref())?.index("last_accessed")?.open_cursor().await? {
                    for _ in 0..num_to_delete {
                        let key: Option<String> = cursor.primary_key()?;
                        if let Some(key) = key {
                            cursor.delete()?.await?;
                            transaction.object_store(bucket)?.delete(key).await?;
                            cursor.advance_by(1).await?;
                        }
                    }
                }
            }

            transaction.commit().await?;
        };

        match result {
            Ok(_) => Ok(()),
            Err(e) => Err(anyhow::anyhow!("{}", e)),
        }
    }

    async fn reset(&self) -> anyhow::Result<()> {
        info!("Resetting client storage");
        let database = self.get_database().await?;
        let result = try {
            for bucket in BUCKETS {
                let bucket_metadata = format!("{}.metadata", bucket);

                let transaction = database.transaction([bucket, bucket_metadata.as_str()]).with_mode(TransactionMode::Readwrite).build()?;
                {
                    transaction.object_store(bucket)?.clear()?;
                    transaction.object_store(bucket_metadata.as_ref())?.clear()?;
                }
                transaction.commit().await?;
            }
        };

        match result {
            Ok(_) => Ok(()),
            Err(e) => Err(anyhow::anyhow!("{}", e)),
        }
    }
}

#[cfg(test)]
pub mod tests {
    extern crate wasm_bindgen_test;
    use crate::client::client_storage::client_storage;
    use wasm_bindgen_test::*;

    wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test]
    async fn add_test() {
        use super::WasmClientStorage;
        client_storage::tests::add_test(WasmClientStorage::new().await.unwrap()).await;
    }

    #[wasm_bindgen_test]
    async fn trim_test() {
        use super::WasmClientStorage;
        client_storage::tests::trim_test(WasmClientStorage::new().await.unwrap()).await;
    }
}

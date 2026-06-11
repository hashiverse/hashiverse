use anyhow::anyhow;
use indexed_db_futures::database::Database;
use indexed_db_futures::prelude::*;
use indexed_db_futures::transaction::TransactionMode;
use indexed_db_futures::KeyPath;
use serde::{Deserialize, Serialize};

const DATABASE_NAME: &str = "hashiverse.local_settings";
const STORE_NAME: &str = "settings";

#[derive(Serialize, Deserialize)]
struct Setting {
    key: String,
    value: String,
}

async fn open_db() -> anyhow::Result<Database> {
    Database::open(DATABASE_NAME)
        .with_version(1u8)
        .with_on_upgrade_needed(|_, db| {
            db.create_object_store(STORE_NAME)
                .with_key_path(KeyPath::from("key"))
                .build()?;
            Ok(())
        })
        .build()
        .map_err(|e| anyhow!("{}", e))?
        .await
        .map_err(|e| anyhow!("{}", e))
}

pub async fn local_settings_get(key: &str) -> anyhow::Result<Option<String>> {
    let db = open_db().await?;
    let transaction = db
        .transaction(STORE_NAME)
        .with_mode(TransactionMode::Readonly)
        .build()
        .map_err(|e| anyhow!("{}", e))?;
    let store = transaction.object_store(STORE_NAME).map_err(|e| anyhow!("{}", e))?;
    let setting: Option<Setting> = store
        .get(key)
        .serde()
        .map_err(|e| anyhow!("{}", e))?
        .await
        .map_err(|e| anyhow!("{}", e))?;
    Ok(setting.map(|s| s.value))
}

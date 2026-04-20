use crate::with_js_context::JsResultExt;
use anyhow::{anyhow, Context};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hashiverse_lib::client::key_locker::key_locker::{KeyLocker, KeyLockerManager, GUEST_CLIENT_ID};
use hashiverse_lib::tools::client_id::ClientId;
use hashiverse_lib::tools::keys::Keys;
use hashiverse_lib::tools::types::{PQCommitmentBytes, Signature, VerificationKeyBytes, SIGNATURE_BYTES};
use indexed_db_futures::database::Database;
use indexed_db_futures::prelude::*;
use indexed_db_futures::transaction::TransactionMode;
use indexed_db_futures::KeyPath;
use js_sys::{JsString, Reflect, Uint8Array};
use log::warn;
use std::sync::Arc;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::js_sys::Object;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Crypto, CryptoKey, SubtleCrypto};

const DATABASE_NAME: &str = "hashiverse.key_locker";
const STORE_NAME: &str = "key";

pub fn get_crypto() -> Result<Crypto, anyhow::Error> {
    let global = js_sys::global();

    if let Some(worker) = global.dyn_ref::<web_sys::WorkerGlobalScope>() {
        return worker.crypto().map_err(|e| anyhow::anyhow!("{:?}", e));
    }

    if let Some(win) = global.dyn_ref::<web_sys::Window>() {
        return win.crypto().map_err(|e| anyhow::anyhow!("{:?}", e));
    }

    anyhow::bail!("Could not find a global crypto object")
}

pub fn get_crypto_subtle() -> Result<SubtleCrypto, anyhow::Error> {
    Ok(get_crypto()?.subtle())
}

async fn get_database() -> anyhow::Result<Database> {
    let result = try {
        let database = Database::open(DATABASE_NAME)
            .with_version(1u8)
            .with_on_blocked(|event| {
                warn!("indexed_db(hashiverse.keys) upgrade blocked: {:?}", event);
                Ok(())
            })
            .with_on_upgrade_needed(|event, db| {
                let old_version = event.old_version() as u64;
                let new_version = event.new_version().map(|v| v as u64);
                warn!("indexed_db upgrade needed from {:?} to {:?}", old_version, new_version);

                match (old_version, new_version) {
                    (0, Some(1)) => {
                        db.create_object_store(STORE_NAME).with_key_path(KeyPath::from("key")).build()?;
                    }
                    _ => {
                        warn!("Unhandled upgrade from indexed_db(hashiverse.keys) old={:?} to new={:?}", old_version, new_version);
                    }
                }

                Ok(())
            })
            .build()?
            .await?;

        database
    };

    match result {
        Ok(x) => Ok(x),
        Err(e) => Err(anyhow::anyhow!("{}", e)),
    }
}

pub struct WasmKeyLocker {
    client_id: ClientId,
    crypto_key: CryptoKey,
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl KeyLocker for WasmKeyLocker {
    fn client_id(&self) -> &ClientId {
        &self.client_id
    }

    async fn sign(&self, data: &[u8]) -> anyhow::Result<Signature> {
        let uint8_view = unsafe { Uint8Array::view(data) };

        let promise = get_crypto_subtle()?.sign_with_str_and_js_u8_array("Ed25519", &self.crypto_key, &uint8_view).with_js_context(|| "sign_with_str_and_js_u8_array")?;

        let result = JsFuture::from(promise).await.with_js_context(|| "await")?;
        let array_buffer = result.dyn_ref::<js_sys::ArrayBuffer>().ok_or_else(|| JsValue::from_str("Not a ArrayBuffer")).with_js_context(|| "dyn_ref")?;
        let array = js_sys::Uint8Array::new(array_buffer);
        if array.length() != (SIGNATURE_BYTES as u32) {
            return Err(anyhow!("sign_with_str_and_js_u8_array result length is not SIGNATURE_BYTES long"));
        }

        let mut bytes: [u8; SIGNATURE_BYTES] = [0u8; SIGNATURE_BYTES];
        array.copy_to(&mut bytes);
        let signature = Signature::from_bytes_exact(bytes);

        Ok(signature)
    }
}

pub struct WasmKeyLockerManager {}
impl KeyLockerManager<WasmKeyLocker> for WasmKeyLockerManager {
    async fn new() -> anyhow::Result<Arc<Self>> {
        Ok(Arc::new(Self {}))
    }

    async fn list(&self) -> anyhow::Result<Vec<String>> {
        let database = get_database().await?;
        let transaction = database.transaction(STORE_NAME).with_mode(TransactionMode::Readonly).build().with_js_context(|| "transaction")?;
        let object_store = transaction.object_store(STORE_NAME).with_js_context(|| "object_store")?;

        let keys = object_store.get_all_keys::<String>().await.with_js_context(|| "get_all_keys")?;
        let keys = keys.into_iter().filter_map(|v| v.ok()).filter(|k| k != GUEST_CLIENT_ID).collect();

        Ok(keys)
    }

    async fn create(&self, key_phrase: String) -> anyhow::Result<Arc<WasmKeyLocker>> {
        let keys = Keys::from_phrase(&key_phrase)?;
        let client_id = ClientId::new(keys.verification_key_bytes, keys.pq_commitment_bytes)?;

        // Prepare the Public Key as the IndexedDB key (IDB Key)
        let key_public = client_id.id.to_hex_str();
        let key_public_js = JsString::from(key_public.clone());

        // The fields to rebuild our ClientID
        let verification_key_js = JsString::from(keys.verification_key_bytes.to_hex());
        let pq_commitment_js = JsString::from(keys.pq_commitment_bytes.to_hex());

        // Encode the private and public keys
        let d_encoded = URL_SAFE_NO_PAD.encode(keys.signature_key.as_ref());
        let x_encoded = URL_SAFE_NO_PAD.encode(keys.verification_key.as_ref());

        // Create the jwk object that represents our ed25519 keypair
        let jwk = Object::new();
        {
            Reflect::set(&jwk, &JsValue::from_str("kty"), &JsValue::from_str("OKP")).with_js_context(|| "set")?;
            Reflect::set(&jwk, &JsValue::from_str("crv"), &JsValue::from_str("Ed25519")).with_js_context(|| "set")?;
            Reflect::set(&jwk, &JsValue::from_str("d"), &JsValue::from_str(&d_encoded)).with_js_context(|| "set")?;
            Reflect::set(&jwk, &JsValue::from_str("x"), &JsValue::from_str(&x_encoded)).with_js_context(|| "set")?;
            Reflect::set(&jwk, &JsValue::from_str("ext"), &JsValue::from_bool(true)).with_js_context(|| "set")?;
        }

        // Import the key
        let promise = get_crypto_subtle()?
            .import_key_with_object(
                "jwk",
                &jwk,
                &JsValue::from_str("Ed25519").unchecked_ref(),
                false, // Make the private key forever inaccessible/unextractable
                &js_sys::Array::of1(&"sign".into()),
            )
            .with_js_context(|| "import_key_with_object")?;

        // Wait on the promise
        let crypto_key_handle: CryptoKey = JsFuture::from(promise).await.with_js_context(|| "import_key_with_object.await")?.into();

        // Save to IndexedDB
        {
            let document = Object::new();
            {
                Reflect::set(&document, &"key".into(), &key_public_js).with_js_context(|| "set_value")?;
                Reflect::set(&document, &"verification_key".into(), &verification_key_js).with_js_context(|| "set_value")?;
                Reflect::set(&document, &"pq_commitment".into(), &pq_commitment_js).with_js_context(|| "set_value")?;
                Reflect::set(&document, &"crypto_key".into(), &crypto_key_handle).with_js_context(|| "set_value")?;
            };

            let database = get_database().await?;
            let transaction = database.transaction(STORE_NAME).with_mode(TransactionMode::Readwrite).build().with_js_context(|| "transaction")?;
            let object_store = transaction.object_store(STORE_NAME).with_js_context(|| "object_store")?;

            object_store.put(document).await.with_js_context(|| "put")?;
            transaction.commit().await.with_js_context(|| "transaction.commit")?;
        }

        // Switch to using this key
        let wasm_key_locker = self.switch(key_public).await?;
        Ok(wasm_key_locker)
    }

    async fn switch(&self, key_public: String) -> anyhow::Result<Arc<WasmKeyLocker>> {
        let key_public_js = JsString::from(key_public);

        let database = get_database().await?;
        let transaction = database.transaction(STORE_NAME).with_mode(TransactionMode::Readonly).build().with_js_context(|| "transaction")?;
        let object_store = transaction.object_store(STORE_NAME).with_js_context(|| "object_store")?;

        let js_value: Option<JsValue> = object_store.get(&key_public_js).await.with_js_context(|| "get")?;
        if let Some(js_value) = js_value {
            let verification_key_js = Reflect::get(&js_value, &"verification_key".into()).with_js_context(|| "get")?;
            let pq_commitment_js = Reflect::get(&js_value, &"pq_commitment".into()).with_js_context(|| "get")?;
            let crypto_key = Reflect::get(&js_value, &"crypto_key".into()).with_js_context(|| "get")?.unchecked_into::<CryptoKey>();

            let verification_key: String = verification_key_js.as_string().context("verification_key is not a string")?;
            let verification_key_bytes = VerificationKeyBytes::from_hex_str(&verification_key)?;
            let pq_commitment: String = pq_commitment_js.as_string().context("verification_key is not a string")?;
            let pq_commitment_bytes = PQCommitmentBytes::from_hex_str(&pq_commitment)?;

            let client_id = ClientId::new(verification_key_bytes, pq_commitment_bytes)?;

            let wasm_key_locker = Arc::new(WasmKeyLocker { client_id, crypto_key });
            return Ok(wasm_key_locker);
        }

        Err(anyhow!("Key not found"))
    }

    async fn delete(&self, key_public: String) -> anyhow::Result<()> {
        let key_public_js = JsString::from(key_public);

        let database = get_database().await?;
        let transaction = database.transaction(STORE_NAME).with_mode(TransactionMode::Readwrite).build().with_js_context(|| "transaction")?;
        let object_store = transaction.object_store(STORE_NAME).with_js_context(|| "object_store")?;

        object_store.delete(&key_public_js).await.with_js_context(|| "delete")?;
        transaction.commit().await.with_js_context(|| "commit")?;

        Ok(())
    }

    async fn reset(&self) -> anyhow::Result<()> {
        let database = get_database().await?;
        let transaction = database.transaction(STORE_NAME).with_mode(TransactionMode::Readwrite).build().with_js_context(|| "transaction")?;
        let object_store = transaction.object_store(STORE_NAME).with_js_context(|| "object_store")?;

        object_store.clear().with_js_context(|| "clear")?;
        transaction.commit().await.with_js_context(|| "commit")?;

        Ok(())
    }
}

#[cfg(test)]
pub mod tests {
    extern crate wasm_bindgen_test;
    use crate::wasm_key_locker::{WasmKeyLocker, WasmKeyLockerManager};
    use hashiverse_lib::client::key_locker::key_locker;
    use wasm_bindgen_test::*;

    wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test]
    async fn add_test() {
        key_locker::tests::add_test::<WasmKeyLocker, WasmKeyLockerManager>().await;
    }
    #[wasm_bindgen_test]
    async fn sign_test() {
        key_locker::tests::sign_test::<WasmKeyLocker, WasmKeyLockerManager>().await;
    }
    #[wasm_bindgen_test]
    async fn guest_client_id_excluded_from_list_test() {
        key_locker::tests::guest_client_id_excluded_from_list_test::<WasmKeyLocker, WasmKeyLockerManager>().await;
    }
}

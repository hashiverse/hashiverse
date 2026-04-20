//! # `SubmitPostV1` — post submission envelope
//!
//! The wire format used to upload a new post from a client to a server responsible for
//! the bucket it targets. [`SubmitPostV1`] pairs:
//!
//! - an Ed25519-signed, brotli-compressed JSON header ([`PostSigningAuthorityV1`]),
//! - the encrypted post body.
//!
//! [`PostSigningAuthorityV1`] also covers ephemeral delegation: a temporary subkey with
//! an explicit expiry and its own PoW salt may sign on the author's behalf, so sensitive
//! identity keys don't need to be loaded into low-trust environments (browser Web
//! Workers, headless automation) to post. The variant enum is open-ended so new
//! signing mechanisms (PQ-only, multi-party, threshold) can be added without breaking
//! existing clients.

use bytes::Bytes;
use crate::tools::types::{Salt, Signature, SignatureKey};
use crate::tools::time::TimeMillis;
use crate::tools::{compression, signing};
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct PostSigningAuthorityDirectV1 {
    pub payload_signature: Signature,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct PostSigningAuthorityEphemeralV1 {
    pub ephemeral_signature_pub: [u8; 32],
    pub expires: TimeMillis,
    pub salt: Salt,
    pub signature: Signature, // sign(hash(ephemeral_signature_pub, expires, salt))
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub enum PostSigningAuthorityV1 {
    PostSigningAuthorityDirectV1(PostSigningAuthorityDirectV1),
    PostSigningAuthorityEphemeralV1(PostSigningAuthorityEphemeralV1),
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct SubmitPostHeaderV1 {
    pub post_signing_authority: PostSigningAuthorityV1,
}

#[derive(Debug, PartialEq, Clone)]
pub struct SubmitPostV1 {
    pub submit_post_header_signature: Signature,
    pub submit_post_header_bytes: Bytes, // SubmitPostHeaderV1
    pub encrypted_post_signature: Signature,
    pub encrypted_post_bytes: Bytes, // PostV1 - encrypted
}

impl SubmitPostV1 {
    pub fn new(
        submit_post_header: &SubmitPostHeaderV1,
        encrypted_post_signature: Signature,
        encrypted_post_bytes: Bytes,
        signature_key: SignatureKey,
    ) -> anyhow::Result<Self> {
        let submit_post_header_json = serde_json::to_string(&submit_post_header)?;
        let submit_post_header_bytes = compression::compress_for_speed(submit_post_header_json.as_bytes())?.to_bytes();
        let submit_post_header_signature = signing::sign(&signature_key, &submit_post_header_bytes);

        Ok(Self {
            submit_post_header_signature,
            submit_post_header_bytes,
            encrypted_post_signature,
            encrypted_post_bytes,
        })
    }

    pub fn submit_post_header_get(&self) -> anyhow::Result<SubmitPostHeaderV1> {
        let header_json_bytes = compression::decompress(&self.submit_post_header_bytes)?.to_bytes();
        Ok(serde_json::from_slice(&header_json_bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use crate::tools::keys::Keys;
    use super::*;
    use serde_json;
    use crate::tools::tools;

    #[tokio::test]
    async fn test_to_from_json() -> anyhow::Result<()> {
        let keys = Keys::from_phrase("this is a test phrase")?;
        
        let mut payload = [0u8; 1024];
        tools::random_fill_bytes(&mut payload);
        
        let header = SubmitPostHeaderV1 {
            post_signing_authority: PostSigningAuthorityV1::PostSigningAuthorityDirectV1(
                PostSigningAuthorityDirectV1 {
                    payload_signature: signing::sign(&keys.signature_key, &payload)
                },
            ),
        };

        let header_json = serde_json::to_string(&header)?;

        log::info!("header_json={}", header_json);

        let header_restored = serde_json::from_str(&header_json)?;
        assert_eq!(header, header_restored);
        
        Ok(())
    }
}

//! # RPC request and response payloads
//!
//! This is the catalogue of *what* peers say to each other. Every application-level
//! operation — ping, bootstrap, peer announce, submit post (claim + commit), submit
//! feedback, get post bundle, get post bundle feedback, heal, cache, fetch URL preview,
//! trending hashtags — has a versioned request and response type defined here, plus a
//! `u16` discriminator in [`PayloadRequestKind`] / [`PayloadResponseKind`] that routes
//! packets to the right handler on the receiving side.
//!
//! ## Design notes
//!
//! - **Tokens**: Two-phase operations (submit post, heal, cache) exchange server-signed
//!   token types (`SubmitPostClaimTokenV1`, `CacheRequestTokenV1`,
//!   `HealPostBundleClaimTokenV1`) so the second phase can prove admission was granted
//!   without the server needing to keep per-client state. Each token embeds the peer's
//!   own [`Peer`] record so verification is self-contained.
//! - **Bulk payloads** (`GetPostBundleResponseV1`, `GetPostBundleFeedbackResponseV1`)
//!   use a custom length-prefixed-JSON + raw-bytes format rather than pure JSON, so
//!   large opaque blobs stream through the
//!   [`crate::tools::bytes_gatherer::BytesGatherer`] without being re-serialised.
//! - **PoW** on any request that carries weight (posting, feedback, fetching URLs) is
//!   carried inline via [`crate::protocol::peer::ClientPow`] credentials so the server
//!   can validate amplification before spending CPU on the payload.

use crate::protocol::peer::Peer;
use crate::tools::buckets::BucketLocation;
use crate::tools::time::TimeMillis;
use crate::tools::types::{Hash, Id, Signature, SignatureKey, VerificationKey, ID_BYTES};
use crate::tools::{hashing, json, signing, tools, BytesGatherer};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};
use strum_macros::{Display, FromRepr};
use crate::anyhow_assert_ge;
use crate::protocol::posting::encoded_post_bundle::EncodedPostBundleHeaderV1;
use crate::protocol::posting::encoded_post_feedback::EncodedPostFeedbackV1;

/// The wire-level discriminator for every RPC request the protocol supports.
///
/// This `u16` value sits in the RPC request header so the server-side dispatcher can route
/// each incoming [`crate::protocol::rpc::RpcRequestPacketRx`] to the correct handler
/// without having to partially decode the payload first. The variants cover the three
/// main subsystems: peer exchange (`PingV1`, `BootstrapV1`, `AnnounceV1`), posting and
/// retrieval (`GetPostBundleV1`, `SubmitPostClaimV1`, `SubmitPostCommitV1`, feedback, heal,
/// cache), and secondary services (`FetchUrlPreviewV1`, `TrendingHashtagsFetchV1`).
///
/// Every variant has a paired [`PayloadResponseKind`]; backwards-compatible additions go at
/// the end of the enum so existing `u16` values do not shift.
#[derive(Debug, Display, PartialEq, Clone, FromRepr)]
#[repr(u16)]
pub enum PayloadRequestKind {
    ErrorV1, // Used solely for testing error handling
    PingV1,
    BootstrapV1,
    AnnounceV1,
    GetPostBundleV1,
    GetPostBundleFeedbackV1,
    SubmitPostClaimV1,
    SubmitPostCommitV1,
    SubmitPostFeedbackV1,
    HealPostBundleClaimV1,
    HealPostBundleCommitV1,
    HealPostBundleFeedbackV1,
    CachePostBundleV1,
    CachePostBundleFeedbackV1,
    FetchUrlPreviewV1,
    TrendingHashtagsFetchV1,
}

impl PayloadRequestKind {
    pub fn from_u16(value: u16) -> anyhow::Result<Self> {
        Self::from_repr(value).ok_or_else(|| anyhow::anyhow!("unknown PayloadRequestKind: {}", value))
    }
}

/// The wire-level discriminator for every RPC response in the protocol.
///
/// Each variant pairs with a [`PayloadRequestKind`] of the same name minus the `Response`
/// suffix (plus the unpaired `ErrorResponseV1`, which any handler may return on failure).
/// The discriminator rides in the response header so the client knows which payload
/// deserializer to use when it parses an [`crate::protocol::rpc::RpcResponsePacketRx`].
///
/// Additions go at the end of the enum to preserve backwards compatibility.
#[derive(Debug, Display, PartialEq, Clone, FromRepr)]
#[repr(u16)]
pub enum PayloadResponseKind {
    ErrorResponseV1, // Can be returned by any RPC call if an error happens on the server
    PingResponseV1,
    BootstrapResponseV1,
    AnnounceResponseV1,
    GetPostBundleResponseV1,
    GetPostBundleFeedbackResponseV1,
    SubmitPostClaimResponseV1,
    SubmitPostCommitResponseV1,
    SubmitPostFeedbackResponseV1,
    HealPostBundleClaimResponseV1,
    HealPostBundleCommitResponseV1,
    HealPostBundleFeedbackResponseV1,
    CachePostBundleResponseV1,
    CachePostBundleFeedbackResponseV1,
    FetchUrlPreviewResponseV1,
    TrendingHashtagsFetchResponseV1,
}

impl PayloadResponseKind {
    pub fn from_u16(value: u16) -> anyhow::Result<Self> {
        Self::from_repr(value).ok_or_else(|| anyhow::anyhow!("unknown PayloadResponseKind: {}", value))
    }
}

/// The universal error response body returned when a handler rejects a request.
///
/// Any RPC handler may return an `ErrorResponseV1` (packaged in a response with
/// [`PayloadResponseKind::ErrorResponseV1`]) instead of its usual success shape. The `code`
/// is a numeric discriminator to let callers branch on error class without string parsing;
/// the `message` is a human-readable explanation for logs and diagnostics.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct ErrorResponseV1 {
    pub code: u16,
    pub message: String,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct PingV1 {}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct PingResponseV1 {
    pub peer: Peer,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct BootstrapV1 {}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct BootstrapResponseV1 {
    pub peers_random: Vec<Peer>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct AnnounceV1 {
    pub peer_self: Peer,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct AnnounceResponseV1 {
    pub peer_self: Peer,
    pub peers_nearest: Vec<Peer>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct GetPostBundleV1 {
    pub bucket_location: BucketLocation,
    pub peers_visited: Vec<Peer>,
    pub already_retrieved_peer_ids: Vec<Id>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct GetPostBundleFeedbackV1 {
    pub bucket_location: BucketLocation,
    pub peers_visited: Vec<Peer>,
    pub already_retrieved_peer_ids: Vec<Id>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct GetPostBundleResponseV1 {
    pub peers_nearer: Vec<Peer>,
    pub cache_request_token: Option<CacheRequestTokenV1>,
    pub post_bundles_cached: Vec<Bytes>,  // Cached EncodedPostBundleV1 from intermediate servers
    pub post_bundle: Option<Bytes>, // Primary EncodedPostBundleV1 from the responsible server
}

impl GetPostBundleResponseV1 {
    pub fn to_bytes_gatherer(&self) -> anyhow::Result<BytesGatherer> {
        let mut bytes_gatherer = BytesGatherer::default();

        tools::write_length_prefixed_json::<Vec<Peer>>(&mut bytes_gatherer, &self.peers_nearer)?;

        match &self.cache_request_token {
            Some(token) => {
                bytes_gatherer.put_u8(1);
                tools::write_length_prefixed_json(&mut bytes_gatherer, token)?;
            }
            None => bytes_gatherer.put_u8(0),
        }

        bytes_gatherer.put_u16(self.post_bundles_cached.len() as u16);
        for bundle in &self.post_bundles_cached {
            bytes_gatherer.put_u32(bundle.len() as u32);
            bytes_gatherer.put_bytes(bundle.clone());
        }

        match &self.post_bundle {
            Some(post_bundle) => { bytes_gatherer.put_u8(1); bytes_gatherer.put_bytes(post_bundle.clone()); }
            None => { bytes_gatherer.put_u8(0); }
        }

        Ok(bytes_gatherer)
    }

    pub fn from_bytes(mut bytes: Bytes) -> anyhow::Result<Self> {
        use bytes::Buf;

        let peers_nearer = tools::read_length_prefixed_json::<Vec<Peer>>(&mut bytes)?;

        if bytes.remaining() < 1 {
            anyhow::bail!("Invalid buffer: missing cache_request_token presence flag");
        }
        let cache_request_token = match bytes.get_u8() {
            0 => None,
            1 => Some(tools::read_length_prefixed_json::<CacheRequestTokenV1>(&mut bytes)?),
            _ => anyhow::bail!("Invalid buffer: unknown cache_request_token presence flag"),
        };

        anyhow_assert_ge!(bytes.remaining(), 2, "Missing post_bundles_cached count");
        let cached_count = bytes.get_u16() as usize;
        let mut post_bundles_cached = Vec::with_capacity(cached_count);
        for _ in 0..cached_count {
            anyhow_assert_ge!(bytes.remaining(), 4, "Missing post_bundles_cached entry length");
            let len = bytes.get_u32() as usize;
            anyhow_assert_ge!(bytes.remaining(), len, "Truncated post_bundles_cached entry");
            post_bundles_cached.push(bytes.split_to(len));
        }

        if bytes.remaining() < 1 {
            anyhow::bail!("Invalid buffer: missing post_bundle presence flag");
        }
        let post_bundle = match bytes.get_u8() {
            0 => None,
            1 => Some(bytes.copy_to_bytes(bytes.remaining())),
            _ => anyhow::bail!("Invalid buffer: unknown post_bundle presence flag"),
        };

        Ok(Self { peers_nearer, cache_request_token, post_bundles_cached, post_bundle })
    }
}
#[derive(Debug, PartialEq, Clone)]
pub struct GetPostBundleFeedbackResponseV1 {
    pub peers_nearer: Vec<Peer>,
    pub cache_request_token: Option<CacheRequestTokenV1>,
    pub post_bundle_feedbacks_cached: Vec<Bytes>, // Cached feedback from originators the client hasn't seen yet (filtered by already_retrieved_peer_ids).
    pub encoded_post_bundle_feedback: Option<Bytes>, // Primary feedback from the responsible server
}

impl GetPostBundleFeedbackResponseV1 {
    pub fn to_bytes_gatherer(&self) -> anyhow::Result<BytesGatherer> {
        let mut bytes_gatherer = BytesGatherer::default();

        tools::write_length_prefixed_json::<Vec<Peer>>(&mut bytes_gatherer, &self.peers_nearer)?;

        match &self.cache_request_token {
            Some(token) => {
                bytes_gatherer.put_u8(1u8);
                tools::write_length_prefixed_json(&mut bytes_gatherer, token)?;
            }
            None => bytes_gatherer.put_u8(0u8),
        }

        bytes_gatherer.put_u16(self.post_bundle_feedbacks_cached.len() as u16);
        for feedback in &self.post_bundle_feedbacks_cached {
            bytes_gatherer.put_u32(feedback.len() as u32);
            bytes_gatherer.put_bytes(feedback.clone());
        }

        match &self.encoded_post_bundle_feedback {
            Some(post_bundle_feedback) => {
                bytes_gatherer.put_u8(1u8);
                bytes_gatherer.put_bytes(post_bundle_feedback.clone());
            }
            None => bytes_gatherer.put_u8(0u8),
        }

        Ok(bytes_gatherer)
    }

    pub fn from_bytes(mut bytes: Bytes) -> anyhow::Result<Self> {
        use bytes::Buf;

        let peers_nearer = tools::read_length_prefixed_json::<Vec<Peer>>(&mut bytes)?;

        if bytes.remaining() < 1 {
            anyhow::bail!("Invalid buffer: missing cache_request_token presence flag");
        }
        let cache_request_token = match bytes.get_u8() {
            0 => None,
            1 => Some(tools::read_length_prefixed_json::<CacheRequestTokenV1>(&mut bytes)?),
            _ => anyhow::bail!("Invalid buffer: unknown cache_request_token presence flag"),
        };

        anyhow_assert_ge!(bytes.remaining(), 2, "Missing post_bundle_feedbacks_cached count");
        let cached_count = bytes.get_u16() as usize;
        let mut post_bundle_feedbacks_cached = Vec::with_capacity(cached_count);
        for _ in 0..cached_count {
            anyhow_assert_ge!(bytes.remaining(), 4, "Missing post_bundle_feedbacks_cached entry length");
            let len = bytes.get_u32() as usize;
            anyhow_assert_ge!(bytes.remaining(), len, "Truncated post_bundle_feedbacks_cached entry");
            post_bundle_feedbacks_cached.push(bytes.split_to(len));
        }

        if bytes.remaining() < 1 {
            anyhow::bail!("Invalid buffer: missing post_bundle_feedback presence flag");
        }
        let post_bundle_feedback = match bytes.get_u8() {
            0 => None,
            1 => Some(bytes.copy_to_bytes(bytes.remaining())),
            _ => anyhow::bail!("Invalid buffer: unknown post_bundle_feedback presence flag"),
        };

        Ok(Self { peers_nearer, cache_request_token, post_bundle_feedbacks_cached, encoded_post_bundle_feedback: post_bundle_feedback })
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct SubmitPostClaimV1 {
    pub bucket_location: BucketLocation,
    pub referenced_post_header_bytes: Option<Bytes>, // For Sequel posts: the original post's bytes_without_body, used to verify same-author
    pub referenced_hashtags: Vec<String>,             // Hashtag strings referenced by this post, for trending tracking
    pub encoded_post_bytes: Bytes,                    // Note that it has just the header of the EncodedPost...
}

impl SubmitPostClaimV1 {
    pub fn new_to_bytes(bucket_location: &BucketLocation, referenced_post_header_bytes: Option<&[u8]>, referenced_hashtags: &[String], encoded_post_bytes_without_body: &[u8]) -> anyhow::Result<Bytes> {
        let mut bytes_gatherer = BytesGatherer::default();
        tools::write_length_prefixed_json(&mut bytes_gatherer, bucket_location)?;
        match referenced_post_header_bytes {
            Some(header_bytes) => {
                bytes_gatherer.put_u32(header_bytes.len() as u32);
                bytes_gatherer.put_slice(header_bytes);
            }
            None => {
                bytes_gatherer.put_u32(0);
            }
        }
        tools::write_length_prefixed_json(&mut bytes_gatherer, &referenced_hashtags)?;
        bytes_gatherer.put_slice(encoded_post_bytes_without_body);
        Ok(bytes_gatherer.to_bytes())
    }

    pub fn from_bytes(bytes: &mut Bytes) -> anyhow::Result<Self> {
        use bytes::Buf;
        let bucket_location = tools::read_length_prefixed_json::<BucketLocation>(bytes)?;

        anyhow::ensure!(bytes.remaining() >= 4, "SubmitPostClaimV1: missing referenced_post_header_bytes length");
        let referenced_post_header_bytes_len = bytes.get_u32() as usize;
        let referenced_post_header_bytes = if referenced_post_header_bytes_len > 0 {
            anyhow::ensure!(bytes.remaining() >= referenced_post_header_bytes_len, "SubmitPostClaimV1: referenced_post_header_bytes length {} exceeds remaining {}", referenced_post_header_bytes_len, bytes.remaining());
            Some(bytes.split_to(referenced_post_header_bytes_len))
        } else {
            None
        };

        let referenced_hashtags = tools::read_length_prefixed_json::<Vec<String>>(bytes)?;

        anyhow::ensure!(bytes.has_remaining(), "SubmitPostClaimV1: missing encoded_post_bytes");
        let encoded_post_bytes = bytes.split_to(bytes.len());

        Ok(Self { bucket_location, referenced_post_header_bytes, referenced_hashtags, encoded_post_bytes })
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct SubmitPostClaimResponseV1 {
    pub peers_nearer: Vec<Peer>,
    pub submit_post_claim_token: Option<SubmitPostClaimTokenV1>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct SubmitPostCommitV1 {
    pub bucket_location: BucketLocation,
    pub submit_post_claim_token: SubmitPostClaimTokenV1,
    pub encoded_post_bytes: Bytes,
}

impl SubmitPostCommitV1 {
    pub fn new_to_bytes(bucket_location: &BucketLocation, submit_post_claim_token: &SubmitPostClaimTokenV1, encoded_post_bytes: &[u8]) -> anyhow::Result<Bytes> {
        let mut bytes_gatherer = BytesGatherer::default();
        tools::write_length_prefixed_json(&mut bytes_gatherer, bucket_location)?;
        tools::write_length_prefixed_json(&mut bytes_gatherer, submit_post_claim_token)?;
        bytes_gatherer.put_slice(encoded_post_bytes);
        Ok(bytes_gatherer.to_bytes())
    }

    pub fn from_bytes(bytes: &mut Bytes) -> anyhow::Result<Self> {
        let bucket_location = tools::read_length_prefixed_json::<BucketLocation>(bytes)?;
        let submit_post_claim_token = tools::read_length_prefixed_json::<SubmitPostClaimTokenV1>(bytes)?;
        let encoded_post_bytes = bytes.split_to(bytes.len());

        Ok(Self {
            bucket_location,
            submit_post_claim_token,
            encoded_post_bytes,
        })
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct SubmitPostCommitResponseV1 {
    pub submit_post_commit_token: SubmitPostCommitTokenV1,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct SubmitPostClaimTokenV1 {
    pub peer: Peer,
    pub bucket_location: BucketLocation,
    pub post_id: Id,
    pub token_signature: Signature,
}

impl SubmitPostClaimTokenV1 {
    fn get_hash_for_signing(peer: &Peer, bucket_location: &BucketLocation, post_id: &Id) -> Hash {
        hashing::hash_multiple(&[peer.id.as_ref(), bucket_location.get_hash_for_signing().as_ref(), post_id.as_ref(), "SubmitPostClaimTokenV1".as_bytes()])
    }

    pub fn new(peer: Peer, bucket_location: BucketLocation, post_id: Id, signature_key: &SignatureKey) -> Self {
        let token_signature = signing::sign(signature_key, Self::get_hash_for_signing(&peer, &bucket_location, &post_id).as_ref());
        Self { peer, bucket_location, post_id, token_signature }
    }

    pub fn verify(&self) -> anyhow::Result<()> {
        self.peer.verify()?;
        let verification_key = VerificationKey::from_bytes(&self.peer.verification_key_bytes)?;
        signing::verify(&verification_key, &self.token_signature, Self::get_hash_for_signing(&self.peer, &self.bucket_location, &self.post_id).as_ref())
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct SubmitPostCommitTokenV1 {
    pub peer: Peer,
    pub bucket_location: BucketLocation,
    pub post_id: Id,
    pub token_signature: Signature,
}

impl SubmitPostCommitTokenV1 {
    fn get_hash_for_signing(peer: &Peer, bucket_location: &BucketLocation, post_id: &Id) -> Hash {
        hashing::hash_multiple(&[peer.id.as_ref(), bucket_location.get_hash_for_signing().as_ref(), post_id.as_ref(), "SubmitPostCommitTokenV1".as_bytes()])
    }

    pub fn new(peer: Peer, bucket_location: BucketLocation, post_id: Id, signature_key: &SignatureKey) -> Self {
        let token_signature = signing::sign(signature_key, Self::get_hash_for_signing(&peer, &bucket_location, &post_id).as_ref());
        Self { peer, bucket_location, post_id, token_signature }
    }

    pub fn verify(&self) -> anyhow::Result<()> {
        self.peer.verify()?;
        let verification_key = VerificationKey::from_bytes(&self.peer.verification_key_bytes)?;
        signing::verify(&verification_key, &self.token_signature, Self::get_hash_for_signing(&self.peer, &self.bucket_location, &self.post_id).as_ref())
    }
}


#[derive(Debug, PartialEq, Clone)]
pub struct SubmitPostFeedbackV1 {
    pub bucket_location: BucketLocation,
    pub encoded_post_feedback: EncodedPostFeedbackV1,
}

impl SubmitPostFeedbackV1 {
    pub fn new_to_bytes(bucket_location: &BucketLocation, encoded_post_feedback: &EncodedPostFeedbackV1) -> anyhow::Result<Bytes> {
        let mut bytes_gatherer = BytesGatherer::default();
        tools::write_length_prefixed_json(&mut bytes_gatherer, bucket_location)?;
        let mut feedback_bytes = BytesMut::new();
        encoded_post_feedback.append_encode_to_bytes(&mut feedback_bytes)?;
        bytes_gatherer.put_bytes(feedback_bytes.freeze());
        Ok(bytes_gatherer.to_bytes())
    }

    pub fn from_bytes(bytes: &mut Bytes) -> anyhow::Result<Self> {
        let bucket_location = tools::read_length_prefixed_json::<BucketLocation>(bytes)?;
        let encoded_post_feedback = EncodedPostFeedbackV1::decode_from_bytes(bytes)?;
        Ok(Self { bucket_location, encoded_post_feedback })
    }

}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct SubmitPostFeedbackResponseV1 {
    pub peers_nearer: Vec<Peer>,
    pub accepted: bool,
}



// ---- Heal Post Bundle (two-phase: claim then commit) ----

#[derive(Debug, PartialEq, Clone)]
pub struct HealPostBundleClaimV1 {
    pub bucket_location: BucketLocation,
    pub donor_header: EncodedPostBundleHeaderV1,
}

impl HealPostBundleClaimV1 {
    pub fn new_to_bytes(donor_header: &EncodedPostBundleHeaderV1, bucket_location: &BucketLocation) -> anyhow::Result<Bytes> {
        let mut g = BytesGatherer::default();
        tools::write_length_prefixed_json(&mut g, bucket_location)?;
        tools::write_length_prefixed_json(&mut g, donor_header)?;
        Ok(g.to_bytes())
    }

    pub fn from_bytes(bytes: &mut Bytes) -> anyhow::Result<Self> {
        let bucket_location = tools::read_length_prefixed_json::<BucketLocation>(bytes)?;
        let donor_header = tools::read_length_prefixed_json::<EncodedPostBundleHeaderV1>(bytes)?;
        Ok(Self { donor_header, bucket_location })
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct HealPostBundleClaimResponseV1 {
    pub needed_post_ids: Vec<Id>,
    pub token: Option<HealPostBundleClaimTokenV1>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct HealPostBundleClaimTokenV1 {
    pub peer: Peer,
    pub bucket_location: BucketLocation,
    pub donor_header_signature: Signature,
    pub needed_post_ids: Vec<Id>,
    pub token_signature: Signature,
}

impl HealPostBundleClaimTokenV1 {
    fn get_hash_for_signing(peer_id: &Id, bucket_location: &BucketLocation, donor_header_signature: &Signature, needed_post_ids: &[Id]) -> Hash {
        let bucket_location_hash = bucket_location.get_hash_for_signing();
        let mut hash_input: Vec<&[u8]> = vec![
            peer_id.as_ref(),
            bucket_location_hash.as_ref(),
            donor_header_signature.as_ref(),
            "HealPostBundleClaimTokenV1".as_bytes(),
        ];
        for id in needed_post_ids {
            hash_input.push(id.as_ref());
        }
        hashing::hash_multiple(&hash_input)
    }

    pub fn new(peer: Peer, bucket_location: BucketLocation, needed_post_ids: Vec<Id>, donor_header_signature: Signature, signature_key: &SignatureKey) -> Self {
        let hash = Self::get_hash_for_signing(&peer.id, &bucket_location, &donor_header_signature, &needed_post_ids);
        let token_signature = signing::sign(signature_key, hash.as_ref());
        Self { peer, bucket_location, donor_header_signature, needed_post_ids, token_signature }
    }

    pub fn verify(&self) -> anyhow::Result<()> {
        self.peer.verify()?;
        self.bucket_location.validate()?;
        let hash = Self::get_hash_for_signing(&self.peer.id, &self.bucket_location, &self.donor_header_signature, &self.needed_post_ids);
        let verification_key = VerificationKey::from_bytes(&self.peer.verification_key_bytes)?;
        signing::verify(&verification_key, &self.token_signature, hash.as_ref())
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct HealPostBundleCommitV1 {
    pub token: HealPostBundleClaimTokenV1,
    pub donor_header: EncodedPostBundleHeaderV1,
    pub encoded_posts_bytes: Bytes,
}

impl HealPostBundleCommitV1 {
    pub fn new_to_bytes(token: &HealPostBundleClaimTokenV1, donor_header: &EncodedPostBundleHeaderV1, encoded_posts_bytes: &[u8]) -> anyhow::Result<Bytes> {
        let mut bytes_gatherer = BytesGatherer::default();
        tools::write_length_prefixed_json(&mut bytes_gatherer, token)?;
        tools::write_length_prefixed_json(&mut bytes_gatherer, donor_header)?;
        bytes_gatherer.put_slice(encoded_posts_bytes);
        Ok(bytes_gatherer.to_bytes())
    }

    pub fn from_bytes(bytes: &mut Bytes) -> anyhow::Result<Self> {
        let token = tools::read_length_prefixed_json::<HealPostBundleClaimTokenV1>(bytes)?;
        let donor_header = tools::read_length_prefixed_json::<EncodedPostBundleHeaderV1>(bytes)?;
        let encoded_posts_bytes = bytes.split_to(bytes.len());
        Ok(Self { token, donor_header, encoded_posts_bytes })
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct HealPostBundleCommitResponseV1 {
    pub accepted: bool,
}

// ---- Heal Post Bundle Feedback (single-phase) ----

#[derive(Debug, PartialEq, Clone)]
pub struct HealPostBundleFeedbackV1 {
    pub location_id: Id,
    pub encoded_post_feedbacks: Vec<EncodedPostFeedbackV1>,
}

impl HealPostBundleFeedbackV1 {
    pub fn new_to_bytes(location_id: &Id, feedbacks: &[EncodedPostFeedbackV1]) -> anyhow::Result<Bytes> {
        let mut bytes = BytesMut::new();
        bytes.put_slice(location_id.as_ref());
        for f in feedbacks {
            f.append_encode_to_bytes(&mut bytes)?;
        }
        Ok(bytes.freeze())
    }

    pub fn from_bytes(bytes: &mut Bytes) -> anyhow::Result<Self> {
        use bytes::Buf;
        anyhow_assert_ge!(bytes.remaining(), ID_BYTES, "Missing location_id in HealPostBundleFeedbackV1");
        let location_id = Id::from_slice(&bytes.split_to(ID_BYTES))?;
        let mut encoded_post_feedbacks = Vec::new();
        while bytes.has_remaining() {
            encoded_post_feedbacks.push(EncodedPostFeedbackV1::decode_from_bytes(&mut *bytes)?);
        }
        Ok(Self { location_id, encoded_post_feedbacks })
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct HealPostBundleFeedbackResponseV1 {
    pub accepted_count: u32,
}


/// Issued by an intermediate server during a GetPostBundle/GetPostBundleFeedback miss or stale hit.
/// The client uploads the bundle to this server after completing its walk.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct CacheRequestTokenV1 {
    pub peer: Peer,
    pub bucket_location: BucketLocation,
    pub expires_at: TimeMillis,
    /// Originator peer IDs already held in this server's cache for this location_id.
    /// The client should only upload a bundle whose originator peer.id is NOT in this list.
    pub already_cached_peer_ids: Vec<Id>,
    pub token_signature: Signature,
}

impl CacheRequestTokenV1 {
    fn get_hash_for_signing(peer_id: &Id, bucket_location: &BucketLocation, expires_at: &TimeMillis, already_cached_peer_ids: &[Id]) -> Hash {
        let expires_at_be = expires_at.encode_be();
        let bucket_location_hash = bucket_location.get_hash_for_signing();
        let mut inputs: Vec<&[u8]> = vec![peer_id.as_ref(), bucket_location_hash.as_ref(), expires_at_be.as_ref(), "CacheRequestToken".as_bytes()];
        for id in already_cached_peer_ids {
            inputs.push(id.as_ref());
        }
        hashing::hash_multiple(&inputs)
    }

    pub fn new(peer: Peer, bucket_location: BucketLocation, expires_at: TimeMillis, already_cached_peer_ids: Vec<Id>, signature_key: &SignatureKey) -> Self {
        let token_signature = signing::sign(signature_key, Self::get_hash_for_signing(&peer.id, &bucket_location, &expires_at, &already_cached_peer_ids).as_ref());
        Self { peer, bucket_location, expires_at, already_cached_peer_ids, token_signature }
    }

    pub fn verify(&self) -> anyhow::Result<()> {
        self.peer.verify()?;
        self.bucket_location.validate()?;
        let verification_key = VerificationKey::from_bytes(&self.peer.verification_key_bytes)?;
        signing::verify(&verification_key, &self.token_signature, Self::get_hash_for_signing(&self.peer.id, &self.bucket_location, &self.expires_at, &self.already_cached_peer_ids).as_ref())
    }

    pub fn is_expired(&self, current_time: TimeMillis) -> bool {
        current_time > self.expires_at
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct CachePostBundleV1 {
    pub token: CacheRequestTokenV1,
    /// One entry per originator — the client uploads all bundles it collected during its walk.
    pub encoded_post_bundles: Vec<Bytes>,
}

impl CachePostBundleV1 {
    pub fn new_to_bytes(token: &CacheRequestTokenV1, encoded_post_bundles: &[&[u8]]) -> anyhow::Result<Bytes> {
        let mut bytes_gatherer = BytesGatherer::default();
        tools::write_length_prefixed_json(&mut bytes_gatherer, token)?;
        bytes_gatherer.put_u16(encoded_post_bundles.len() as u16);
        for bundle in encoded_post_bundles {
            bytes_gatherer.put_u32(bundle.len() as u32);
            bytes_gatherer.put_slice(bundle);
        }
        Ok(bytes_gatherer.to_bytes())
    }

    pub fn from_bytes(bytes: &mut Bytes) -> anyhow::Result<Self> {
        let token = tools::read_length_prefixed_json::<CacheRequestTokenV1>(bytes)?;
        anyhow_assert_ge!(bytes.remaining(), 2, "Missing encoded_post_bundles count");
        let count = bytes.get_u16() as usize;
        let mut encoded_post_bundles = Vec::with_capacity(count);
        for _ in 0..count {
            anyhow_assert_ge!(bytes.remaining(), 4, "Missing encoded_post_bundle entry length");
            let len = bytes.get_u32() as usize;
            anyhow_assert_ge!(bytes.remaining(), len, "Truncated encoded_post_bundle entry");
            encoded_post_bundles.push(bytes.split_to(len));
        }
        Ok(Self { token, encoded_post_bundles })
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct CachePostBundleResponseV1 {
    pub accepted: bool,
}

#[derive(Debug, PartialEq, Clone)]
pub struct CachePostBundleFeedbackV1 {
    pub token: CacheRequestTokenV1,
    pub encoded_post_bundle_feedback_bytes: Bytes,
}

impl CachePostBundleFeedbackV1 {
    pub fn new_to_bytes(token: &CacheRequestTokenV1, encoded_post_bundle_feedback_bytes: &[u8]) -> anyhow::Result<Bytes> {
        let mut bytes_gatherer = BytesGatherer::default();
        tools::write_length_prefixed_json(&mut bytes_gatherer, token)?;
        bytes_gatherer.put_slice(encoded_post_bundle_feedback_bytes);
        Ok(bytes_gatherer.to_bytes())
    }

    pub fn from_bytes(bytes: &mut Bytes) -> anyhow::Result<Self> {
        let token = tools::read_length_prefixed_json::<CacheRequestTokenV1>(bytes)?;
        let encoded_post_bundle_feedback_bytes = bytes.split_to(bytes.len());
        Ok(Self { token, encoded_post_bundle_feedback_bytes })
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct CachePostBundleFeedbackResponseV1 {
    pub accepted: bool,
}

/// Client → Server: fetch Open Graph metadata for `url`
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct FetchUrlPreviewV1 {
    pub url: String,
}

impl FetchUrlPreviewV1 {
    pub fn new_to_bytes(url: &str) -> anyhow::Result<Bytes> {
        let mut bytes_gatherer = BytesGatherer::default();
        tools::write_length_prefixed_json(&mut bytes_gatherer, &FetchUrlPreviewV1 { url: url.to_string() })?;
        Ok(bytes_gatherer.to_bytes())
    }

    pub fn from_bytes(bytes: &mut Bytes) -> anyhow::Result<Self> {
        tools::read_length_prefixed_json(bytes)
    }
}

/// Server → Client: extracted preview metadata
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct FetchUrlPreviewResponseV1 {
    pub url: String,
    pub title: String,
    pub description: String,
    pub image_url: String,
}

impl FetchUrlPreviewResponseV1 {
    pub fn to_bytes(&self) -> anyhow::Result<Bytes> {
        json::struct_to_bytes(self)
    }

    pub fn from_bytes(bytes: &Bytes) -> anyhow::Result<Self> {
        json::bytes_to_struct(bytes)
    }
}

/// Client → Server: fetch trending hashtags
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct TrendingHashtagsFetchV1 {
    pub limit: u16,
}

impl TrendingHashtagsFetchV1 {
    pub fn new_to_bytes(limit: u16) -> anyhow::Result<Bytes> {
        let mut bytes_gatherer = BytesGatherer::default();
        tools::write_length_prefixed_json(&mut bytes_gatherer, &TrendingHashtagsFetchV1 { limit })?;
        Ok(bytes_gatherer.to_bytes())
    }

    pub fn from_bytes(bytes: &mut Bytes) -> anyhow::Result<Self> {
        tools::read_length_prefixed_json(bytes)
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct TrendingHashtagV1 {
    pub hashtag: String,
    pub count: u64,
}

/// Server → Client: trending hashtags with unique author counts
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct TrendingHashtagsFetchResponseV1 {
    pub trending_hashtags: Vec<TrendingHashtagV1>,
}

impl TrendingHashtagsFetchResponseV1 {
    pub fn to_bytes(&self) -> anyhow::Result<Bytes> {
        json::struct_to_bytes(self)
    }

    pub fn from_bytes(bytes: &Bytes) -> anyhow::Result<Self> {
        json::bytes_to_struct(bytes)
    }
}

#[cfg(test)]
mod tests {
    use crate::protocol::payload::payload::{GetPostBundleResponseV1, HealPostBundleClaimResponseV1, HealPostBundleClaimTokenV1, HealPostBundleClaimV1, HealPostBundleCommitResponseV1, HealPostBundleCommitV1, SubmitPostClaimTokenV1, SubmitPostCommitTokenV1};
    use crate::protocol::posting::encoded_post_bundle::{EncodedPostBundleHeaderV1, EncodedPostBundleV1};
    use crate::tools::server_id::ServerId;
    use crate::tools::buckets::{BucketLocation, BucketType};
    use crate::tools::time::{TimeMillis, MILLIS_IN_HOUR};
    use crate::tools::time_provider::time_provider::{RealTimeProvider, TimeProvider};
    use crate::tools::{config, json, tools};
    use crate::tools::types::{Id, Pow, Signature, ID_BYTES};
    use std::collections::HashSet;
    use crate::tools::parallel_pow_generator::StubParallelPowGenerator;
    use bytes::Bytes;

    #[tokio::test]
    #[allow(non_snake_case)]
    async fn test_to_from_GetPostBundleResponseV1() -> anyhow::Result<()> {
        let time_provider = RealTimeProvider::default();
        let pow_generator = StubParallelPowGenerator::new();
        let server_id = ServerId::new("own_pow", &time_provider, Pow(4), true, &pow_generator).await?;
        let peer = server_id.to_peer(&time_provider)?;

        let mut encoded_posts_ids = Vec::new();
        let mut encoded_posts_lengths = Vec::new();
        let mut encoded_posts_bytes = Vec::new();

        for i in 0..16 {
            let len = (i + 1) * 128;
            let mut encoded_post_bytes = tools::random_bytes(len);
            let post_id = Id::from_slice(&encoded_post_bytes[0..ID_BYTES])?;
            encoded_posts_ids.push(post_id);
            encoded_posts_lengths.push(encoded_post_bytes.len());
            encoded_posts_bytes.append(&mut encoded_post_bytes);
        }

        let header = EncodedPostBundleHeaderV1 {
            time_millis: time_provider.current_time_millis(),
            location_id: Id::random(),
            overflowed: false,
            sealed: false,
            num_posts: 0,
            encoded_post_ids: encoded_posts_ids,
            encoded_post_lengths: encoded_posts_lengths,
            encoded_post_healed: HashSet::new(),
            peer,
            signature: Signature::zero(),
        };

        let peers_nearer = {
            vec![
                ServerId::new("own_pow", &time_provider, Pow(4), true, &pow_generator).await?.to_peer(&time_provider)?,
                ServerId::new("own_pow", &time_provider, Pow(4), true, &pow_generator).await?.to_peer(&time_provider)?,
                ServerId::new("own_pow", &time_provider, Pow(4), true, &pow_generator).await?.to_peer(&time_provider)?,
            ]
        };

        let response = GetPostBundleResponseV1 {
            peers_nearer,
            cache_request_token: None,
            post_bundles_cached: vec![],
            post_bundle: Some((EncodedPostBundleV1 { header, encoded_posts_bytes: Bytes::from(encoded_posts_bytes) }).to_bytes()?),
        };

        let encoded = response.to_bytes_gatherer()?.to_bytes();
        let response_decoded = GetPostBundleResponseV1::from_bytes(encoded)?;

        assert_eq!(response_decoded, response);

        Ok(())
    }

    async fn make_signed_header(time_provider: &RealTimeProvider) -> anyhow::Result<(EncodedPostBundleHeaderV1, ServerId)> {
        let pow_generator = StubParallelPowGenerator::new();
        let server_id = ServerId::new("own_pow", time_provider, config::SERVER_KEY_POW_MIN, true, &pow_generator).await?;
        let peer = server_id.to_peer(time_provider)?;
        let num_posts: u8 = 4;
        let mut header = EncodedPostBundleHeaderV1 {
            time_millis: time_provider.current_time_millis(),
            location_id: Id::random(),
            overflowed: false,
            sealed: false,
            num_posts,
            encoded_post_ids: (0..num_posts).map(|_| Id::random()).collect(),
            encoded_post_lengths: (0..num_posts).map(|i| (i as usize + 1) * 64).collect(),
            encoded_post_healed: HashSet::new(),
            peer,
            signature: Signature::zero(),
        };
        header.signature_generate(&server_id.keys.signature_key)?;
        Ok((header, server_id))
    }

    fn make_bucket_location() -> anyhow::Result<BucketLocation> {
        BucketLocation::new(BucketType::User, Id::random(), MILLIS_IN_HOUR, TimeMillis(1_700_000_000_000))
    }

    #[tokio::test]
    #[allow(non_snake_case)]
    async fn test_SubmitPostClaimTokenV1_verify() -> anyhow::Result<()> {
        let time_provider = RealTimeProvider::default();
        let pow_generator = StubParallelPowGenerator::new();
        let server_id = ServerId::new("own_pow", &time_provider, Pow(4), true, &pow_generator).await?;
        let peer = server_id.to_peer(&time_provider)?;

        let token = SubmitPostClaimTokenV1::new(peer, make_bucket_location()?, Id::random(), &server_id.keys.signature_key);
        token.verify()?;
        Ok(())
    }

    #[tokio::test]
    #[allow(non_snake_case)]
    async fn test_SubmitPostCommitTokenV1_verify() -> anyhow::Result<()> {
        let time_provider = RealTimeProvider::default();
        let pow_generator = StubParallelPowGenerator::new();
        let server_id = ServerId::new("own_pow", &time_provider, Pow(4), true, &pow_generator).await?;
        let peer = server_id.to_peer(&time_provider)?;

        let token = SubmitPostCommitTokenV1::new(peer, make_bucket_location()?, Id::random(), &server_id.keys.signature_key);
        token.verify()?;
        Ok(())
    }

    #[tokio::test]
    #[allow(non_snake_case)]
    async fn test_to_from_HealPostBundleClaimV1() -> anyhow::Result<()> {
        let time_provider = RealTimeProvider::default();
        let (header, _) = make_signed_header(&time_provider).await?;

        let bucket_location = make_bucket_location()?;
        let encoded = HealPostBundleClaimV1::new_to_bytes(&header, &bucket_location)?;
        let mut bytes = encoded;
        let decoded = HealPostBundleClaimV1::from_bytes(&mut bytes)?;

        assert_eq!(decoded.donor_header, header);
        assert_eq!(decoded.bucket_location, bucket_location);
        Ok(())
    }

    #[tokio::test]
    #[allow(non_snake_case)]
    async fn test_to_from_HealPostBundleClaimResponseV1() -> anyhow::Result<()> {
        let time_provider = RealTimeProvider::default();
        let (header, server_id) = make_signed_header(&time_provider).await?;
        let peer = server_id.to_peer(&time_provider)?;

        let needed_post_ids = header.encoded_post_ids[0..2].to_vec();
        let token = HealPostBundleClaimTokenV1::new(
            peer,
            make_bucket_location()?,
            needed_post_ids.clone(),
            header.signature,
            &server_id.keys.signature_key,
        );

        let response = HealPostBundleClaimResponseV1 { needed_post_ids, token: Some(token) };

        let encoded = json::struct_to_bytes(&response)?;
        let decoded = json::bytes_to_struct::<HealPostBundleClaimResponseV1>(&encoded)?;

        assert_eq!(decoded, response);
        Ok(())
    }

    #[tokio::test]
    #[allow(non_snake_case)]
    async fn test_HealPostBundleClaimTokenV1_verify() -> anyhow::Result<()> {
        let time_provider = RealTimeProvider::default();
        let (header, server_id) = make_signed_header(&time_provider).await?;
        let peer = server_id.to_peer(&time_provider)?;

        let token = HealPostBundleClaimTokenV1::new(
            peer,
            make_bucket_location()?,
            header.encoded_post_ids.clone(),
            header.signature,
            &server_id.keys.signature_key,
        );

        token.verify()?;
        Ok(())
    }

    #[tokio::test]
    #[allow(non_snake_case)]
    async fn test_to_from_HealPostBundleCommitV1() -> anyhow::Result<()> {
        let time_provider = RealTimeProvider::default();
        let (header, server_id) = make_signed_header(&time_provider).await?;
        let peer = server_id.to_peer(&time_provider)?;

        let token = HealPostBundleClaimTokenV1::new(
            peer,
            make_bucket_location()?,
            header.encoded_post_ids.clone(),
            header.signature,
            &server_id.keys.signature_key,
        );

        let post_bytes = tools::random_bytes(512);
        let encoded = HealPostBundleCommitV1::new_to_bytes(&token, &header, &post_bytes)?;
        let mut bytes = encoded;
        let decoded = HealPostBundleCommitV1::from_bytes(&mut bytes)?;

        assert_eq!(decoded.token, token);
        assert_eq!(decoded.donor_header, header);
        assert_eq!(decoded.encoded_posts_bytes.as_ref(), post_bytes.as_slice());
        Ok(())
    }

    #[tokio::test]
    #[allow(non_snake_case)]
    async fn test_to_from_HealPostBundleCommitResponseV1() -> anyhow::Result<()> {
        for accepted in [true, false] {
            let response = HealPostBundleCommitResponseV1 { accepted };
            let encoded = json::struct_to_bytes(&response)?;
            let decoded = json::bytes_to_struct::<HealPostBundleCommitResponseV1>(&encoded)?;
            assert_eq!(decoded, response);
        }
        Ok(())
    }

    #[tokio::test]
    #[allow(non_snake_case)]
    async fn test_CacheRequestTokenV1_verify() -> anyhow::Result<()> {
        use crate::protocol::payload::payload::CacheRequestTokenV1;
        use crate::tools::time::MILLIS_IN_MINUTE;

        let time_provider = RealTimeProvider::default();
        let pow_generator = StubParallelPowGenerator::new();
        let server_id = ServerId::new("own_pow", &time_provider, Pow(4), true, &pow_generator).await?;
        let peer = server_id.to_peer(&time_provider)?;
        let bucket_location = make_bucket_location()?;
        let expires_at = time_provider.current_time_millis() + MILLIS_IN_MINUTE;
        let already_cached_peer_ids = vec![Id::random(), Id::random()];

        let token = CacheRequestTokenV1::new(peer, bucket_location, expires_at, already_cached_peer_ids, &server_id.keys.signature_key);
        token.verify()?;
        Ok(())
    }

    #[tokio::test]
    #[allow(non_snake_case)]
    async fn test_to_from_CachePostBundleV1() -> anyhow::Result<()> {
        use crate::protocol::payload::payload::{CachePostBundleV1, CacheRequestTokenV1};
        use crate::tools::time::MILLIS_IN_MINUTE;

        let time_provider = RealTimeProvider::default();
        let pow_generator = StubParallelPowGenerator::new();
        let server_id = ServerId::new("own_pow", &time_provider, Pow(4), true, &pow_generator).await?;
        let peer = server_id.to_peer(&time_provider)?;
        let bucket_location = make_bucket_location()?;
        let expires_at = time_provider.current_time_millis() + MILLIS_IN_MINUTE;
        let token = CacheRequestTokenV1::new(peer, bucket_location, expires_at, vec![], &server_id.keys.signature_key);

        // Multiple bundles — one per originator
        let bundle_a = tools::random_bytes(512);
        let bundle_b = tools::random_bytes(256);
        let bundle_c = tools::random_bytes(1024);
        let bundles: &[&[u8]] = &[&bundle_a, &bundle_b, &bundle_c];

        let encoded = CachePostBundleV1::new_to_bytes(&token, bundles)?;
        let mut bytes = encoded;
        let decoded = CachePostBundleV1::from_bytes(&mut bytes)?;

        assert_eq!(decoded.token, token);
        assert_eq!(decoded.encoded_post_bundles.len(), 3);
        assert_eq!(decoded.encoded_post_bundles[0].as_ref(), bundle_a.as_slice());
        assert_eq!(decoded.encoded_post_bundles[1].as_ref(), bundle_b.as_slice());
        assert_eq!(decoded.encoded_post_bundles[2].as_ref(), bundle_c.as_slice());
        Ok(())
    }

    #[tokio::test]
    #[allow(non_snake_case)]
    async fn test_to_from_CachePostBundleV1_empty() -> anyhow::Result<()> {
        use crate::protocol::payload::payload::{CachePostBundleV1, CacheRequestTokenV1};
        use crate::tools::time::MILLIS_IN_MINUTE;

        let time_provider = RealTimeProvider::default();
        let pow_generator = StubParallelPowGenerator::new();
        let server_id = ServerId::new("own_pow", &time_provider, Pow(4), true, &pow_generator).await?;
        let peer = server_id.to_peer(&time_provider)?;
        let bucket_location = make_bucket_location()?;
        let expires_at = time_provider.current_time_millis() + MILLIS_IN_MINUTE;
        let token = CacheRequestTokenV1::new(peer, bucket_location, expires_at, vec![], &server_id.keys.signature_key);

        let encoded = CachePostBundleV1::new_to_bytes(&token, &[])?;
        let mut bytes = encoded;
        let decoded = CachePostBundleV1::from_bytes(&mut bytes)?;

        assert_eq!(decoded.token, token);
        assert!(decoded.encoded_post_bundles.is_empty());
        Ok(())
    }

    #[tokio::test]
    #[allow(non_snake_case)]
    async fn test_to_from_CachePostBundleFeedbackV1() -> anyhow::Result<()> {
        use crate::protocol::payload::payload::{CachePostBundleFeedbackV1, CacheRequestTokenV1};
        use crate::tools::time::MILLIS_IN_MINUTE;

        let time_provider = RealTimeProvider::default();
        let pow_generator = StubParallelPowGenerator::new();
        let server_id = ServerId::new("own_pow", &time_provider, Pow(4), true, &pow_generator).await?;
        let peer = server_id.to_peer(&time_provider)?;
        let bucket_location = make_bucket_location()?;
        let expires_at = time_provider.current_time_millis() + MILLIS_IN_MINUTE;
        let already_cached_peer_ids = vec![Id::random()];
        let token = CacheRequestTokenV1::new(peer, bucket_location, expires_at, already_cached_peer_ids, &server_id.keys.signature_key);

        let feedback_bytes = tools::random_bytes(256);
        let encoded = CachePostBundleFeedbackV1::new_to_bytes(&token, &feedback_bytes)?;
        let mut bytes = encoded;
        let decoded = CachePostBundleFeedbackV1::from_bytes(&mut bytes)?;

        assert_eq!(decoded.token, token);
        assert_eq!(decoded.encoded_post_bundle_feedback_bytes.as_ref(), feedback_bytes.as_slice());
        Ok(())
    }

    #[tokio::test]
    #[allow(non_snake_case)]
    async fn test_to_from_FetchUrlPreviewV1() -> anyhow::Result<()> {
        use crate::protocol::payload::payload::FetchUrlPreviewV1;

        let url = "https://example.com/article?q=1#section";
        let encoded = FetchUrlPreviewV1::new_to_bytes(url)?;
        let mut bytes = encoded;
        let decoded = FetchUrlPreviewV1::from_bytes(&mut bytes)?;

        assert_eq!(decoded.url, url);
        Ok(())
    }

    #[tokio::test]
    #[allow(non_snake_case)]
    async fn test_to_from_FetchUrlPreviewResponseV1() -> anyhow::Result<()> {
        use crate::protocol::payload::payload::FetchUrlPreviewResponseV1;

        let response = FetchUrlPreviewResponseV1 {
            url: "https://example.com/canonical".to_string(),
            title: "Example Title".to_string(),
            description: "A short description.".to_string(),
            image_url: "https://example.com/image.png".to_string(),
        };

        let encoded = response.to_bytes()?;
        let decoded = FetchUrlPreviewResponseV1::from_bytes(&encoded)?;

        assert_eq!(decoded, response);
        Ok(())
    }

    // ── Robustness tests: CachePostBundleV1::from_bytes ──

    #[test]
    #[allow(non_snake_case)]
    fn test_CachePostBundleV1_from_bytes_empty_input() {
        use crate::protocol::payload::payload::CachePostBundleV1;
        let mut bytes = Bytes::new();
        assert!(CachePostBundleV1::from_bytes(&mut bytes).is_err());
    }

    #[test]
    #[allow(non_snake_case)]
    fn test_CachePostBundleV1_from_bytes_garbage_input() {
        use crate::protocol::payload::payload::CachePostBundleV1;
        let mut bytes = Bytes::from_static(&[0xff; 64]);
        assert!(CachePostBundleV1::from_bytes(&mut bytes).is_err());
    }

    #[test]
    #[allow(non_snake_case)]
    fn test_CachePostBundleV1_from_bytes_truncated_count() {
        use crate::protocol::payload::payload::CachePostBundleV1;
        // Valid length-prefixed JSON for an empty-ish struct won't work here since CacheRequestTokenV1
        // has required fields. But a single byte after a valid-looking length prefix will fail at JSON parse.
        // The simplest test: just one byte (not enough for the length prefix u64).
        let mut bytes = Bytes::from_static(&[0x01]);
        assert!(CachePostBundleV1::from_bytes(&mut bytes).is_err());
    }

    // ── Robustness tests: GetPostBundleResponseV1::from_bytes ──

    #[test]
    #[allow(non_snake_case)]
    fn test_GetPostBundleResponseV1_from_bytes_empty_input() {
        let bytes = Bytes::new();
        assert!(GetPostBundleResponseV1::from_bytes(bytes).is_err());
    }

    #[test]
    #[allow(non_snake_case)]
    fn test_GetPostBundleResponseV1_from_bytes_garbage_input() {
        let bytes = Bytes::from_static(&[0xff; 128]);
        assert!(GetPostBundleResponseV1::from_bytes(bytes).is_err());
    }

    // ── Robustness tests: HealPostBundleClaimV1::from_bytes ──

    #[test]
    #[allow(non_snake_case)]
    fn test_HealPostBundleClaimV1_from_bytes_empty_input() {
        let mut bytes = Bytes::new();
        assert!(HealPostBundleClaimV1::from_bytes(&mut bytes).is_err());
    }

    #[test]
    #[allow(non_snake_case)]
    fn test_HealPostBundleClaimV1_from_bytes_garbage_input() {
        let mut bytes = Bytes::from_static(&[0xff; 64]);
        assert!(HealPostBundleClaimV1::from_bytes(&mut bytes).is_err());
    }

    // ── Robustness tests: HealPostBundleCommitV1::from_bytes ──

    #[test]
    #[allow(non_snake_case)]
    fn test_HealPostBundleCommitV1_from_bytes_empty_input() {
        let mut bytes = Bytes::new();
        assert!(HealPostBundleCommitV1::from_bytes(&mut bytes).is_err());
    }

    #[test]
    #[allow(non_snake_case)]
    fn test_HealPostBundleCommitV1_from_bytes_garbage_input() {
        let mut bytes = Bytes::from_static(&[0xff; 64]);
        assert!(HealPostBundleCommitV1::from_bytes(&mut bytes).is_err());
    }

    // ── Robustness tests: FetchUrlPreviewV1::from_bytes ──

    #[test]
    #[allow(non_snake_case)]
    fn test_FetchUrlPreviewV1_from_bytes_empty_input() {
        use crate::protocol::payload::payload::FetchUrlPreviewV1;
        let mut bytes = Bytes::new();
        assert!(FetchUrlPreviewV1::from_bytes(&mut bytes).is_err());
    }

    // ── Robustness tests: FetchUrlPreviewResponseV1::from_bytes ──

    #[test]
    #[allow(non_snake_case)]
    fn test_FetchUrlPreviewResponseV1_from_bytes_empty_input() {
        use crate::protocol::payload::payload::FetchUrlPreviewResponseV1;
        let bytes = Bytes::new();
        assert!(FetchUrlPreviewResponseV1::from_bytes(&bytes).is_err());
    }

    #[test]
    #[allow(non_snake_case)]
    fn test_FetchUrlPreviewResponseV1_from_bytes_garbage_input() {
        use crate::protocol::payload::payload::FetchUrlPreviewResponseV1;
        let bytes = Bytes::from_static(&[0xff; 64]);
        assert!(FetchUrlPreviewResponseV1::from_bytes(&bytes).is_err());
    }

    // ── Robustness tests: SubmitPostClaimV1::from_bytes ──

    #[test]
    #[allow(non_snake_case)]
    fn test_SubmitPostClaimV1_from_bytes_empty_input() {
        use crate::protocol::payload::payload::SubmitPostClaimV1;
        let mut bytes = Bytes::new();
        assert!(SubmitPostClaimV1::from_bytes(&mut bytes).is_err());
    }

    #[test]
    #[allow(non_snake_case)]
    fn test_SubmitPostClaimV1_from_bytes_garbage_input() {
        use crate::protocol::payload::payload::SubmitPostClaimV1;
        let mut bytes = Bytes::from_static(&[0xff; 64]);
        assert!(SubmitPostClaimV1::from_bytes(&mut bytes).is_err());
    }

    // ── Robustness tests: SubmitPostCommitV1::from_bytes ──

    #[test]
    #[allow(non_snake_case)]
    fn test_SubmitPostCommitV1_from_bytes_empty_input() {
        use crate::protocol::payload::payload::SubmitPostCommitV1;
        let mut bytes = Bytes::new();
        assert!(SubmitPostCommitV1::from_bytes(&mut bytes).is_err());
    }

    // ── Robustness tests: SubmitPostFeedbackV1::from_bytes ──

    #[test]
    #[allow(non_snake_case)]
    fn test_SubmitPostFeedbackV1_from_bytes_empty_input() {
        use crate::protocol::payload::payload::SubmitPostFeedbackV1;
        let mut bytes = Bytes::new();
        assert!(SubmitPostFeedbackV1::from_bytes(&mut bytes).is_err());
    }

    // ── Robustness tests: GetPostBundleFeedbackResponseV1::from_bytes ──

    #[test]
    #[allow(non_snake_case)]
    fn test_GetPostBundleFeedbackResponseV1_from_bytes_empty_input() {
        use crate::protocol::payload::payload::GetPostBundleFeedbackResponseV1;
        let bytes = Bytes::new();
        assert!(GetPostBundleFeedbackResponseV1::from_bytes(bytes).is_err());
    }

    // ── Robustness tests: CachePostBundleFeedbackV1::from_bytes ──

    #[test]
    #[allow(non_snake_case)]
    fn test_CachePostBundleFeedbackV1_from_bytes_empty_input() {
        use crate::protocol::payload::payload::CachePostBundleFeedbackV1;
        let mut bytes = Bytes::new();
        assert!(CachePostBundleFeedbackV1::from_bytes(&mut bytes).is_err());
    }

    #[cfg(not(target_arch = "wasm32"))]
    mod bolero_fuzz {
        use bytes::Bytes;

        #[test]
        #[allow(non_snake_case)]
        fn fuzz_CachePostBundleV1_from_bytes() {
            use crate::protocol::payload::payload::CachePostBundleV1;
            bolero::check!().for_each(|data: &[u8]| {
                let mut bytes = Bytes::copy_from_slice(data);
                let _ = CachePostBundleV1::from_bytes(&mut bytes);
            });
        }

        #[test]
        #[allow(non_snake_case)]
        fn fuzz_GetPostBundleResponseV1_from_bytes() {
            use crate::protocol::payload::payload::GetPostBundleResponseV1;
            bolero::check!().for_each(|data: &[u8]| {
                let _ = GetPostBundleResponseV1::from_bytes(Bytes::copy_from_slice(data));
            });
        }

        #[test]
        #[allow(non_snake_case)]
        fn fuzz_GetPostBundleFeedbackResponseV1_from_bytes() {
            use crate::protocol::payload::payload::GetPostBundleFeedbackResponseV1;
            bolero::check!().for_each(|data: &[u8]| {
                let _ = GetPostBundleFeedbackResponseV1::from_bytes(Bytes::copy_from_slice(data));
            });
        }

        #[test]
        #[allow(non_snake_case)]
        fn fuzz_SubmitPostClaimV1_from_bytes() {
            use crate::protocol::payload::payload::SubmitPostClaimV1;
            bolero::check!().for_each(|data: &[u8]| {
                let mut bytes = Bytes::copy_from_slice(data);
                let _ = SubmitPostClaimV1::from_bytes(&mut bytes);
            });
        }

        #[test]
        #[allow(non_snake_case)]
        fn fuzz_HealPostBundleClaimV1_from_bytes() {
            use crate::protocol::payload::payload::HealPostBundleClaimV1;
            bolero::check!().for_each(|data: &[u8]| {
                let mut bytes = Bytes::copy_from_slice(data);
                let _ = HealPostBundleClaimV1::from_bytes(&mut bytes);
            });
        }

        #[test]
        #[allow(non_snake_case)]
        fn fuzz_HealPostBundleCommitV1_from_bytes() {
            use crate::protocol::payload::payload::HealPostBundleCommitV1;
            bolero::check!().for_each(|data: &[u8]| {
                let mut bytes = Bytes::copy_from_slice(data);
                let _ = HealPostBundleCommitV1::from_bytes(&mut bytes);
            });
        }
    }
}

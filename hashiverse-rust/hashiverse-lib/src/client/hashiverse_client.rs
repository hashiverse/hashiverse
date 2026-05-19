//! # Top-level client API
//!
//! The one thing every external consumer of `hashiverse-lib` ends up holding: a
//! [`HashiverseClient`] instance. The server binary's cache role, the WASM browser client,
//! the Python wrapper, and the integration-test harness all build on top of this struct.
//!
//! `HashiverseClient` wires together the rest of the `client` module — peer tracking,
//! post/feedback bundle management, timelines, the meta-post system, the key locker — and
//! exposes the small set of verbs a social-network client actually performs:
//!
//! - submit posts, submit feedback, fetch URL previews, fetch trending hashtags;
//! - walk per-user / per-hashtag / per-mention timelines;
//! - manage the logged-in account (via [`crate::client::key_locker`]);
//! - read and update the profile ([`crate::client::meta_post`]).
//!
//! Every dependency that varies by environment (clock, network, PoW engine, storage, key
//! locker) is injected through [`crate::tools::runtime_services::RuntimeServices`] and
//! pluggable traits so the same struct works unchanged on native, in the browser (WASM),
//! and in deterministic in-memory integration tests.

use crate::client::args::Args;
use crate::client::client_storage::client_storage::{ClientStorage, BUCKETS, BUCKET_TRIMS};
use crate::client::key_locker::key_locker::KeyLocker;
use crate::client::meta_post::meta_post::MetaPost;
use crate::client::meta_post::meta_post_manager::MetaPostManager;
use crate::client::peer_tracker::peer_tracker::PeerTracker;
use crate::client::post_bundle::live_post_bundle_manager::LivePostBundleManager;
use crate::client::post_bundle::post_bundle_manager::PostBundleManager;
use crate::client::post_bundle::posting;
use crate::client::timeline::recent_posts_pen::RecentPostsPen;
use crate::client::timeline::single_timeline::SingleTimeline;
use crate::protocol::posting::encoded_post::EncodedPostV1;
use crate::protocol::posting::encoded_post_feedback::EncodedPostFeedbackV1;
use crate::tools::buckets::{bucket_durations_for_type, generate_bucket_location, BucketLocation, BucketType};
use crate::tools::client_id::ClientId;
use crate::tools::runtime_services::RuntimeServices;
use crate::tools::types::Id;
use bytes::Bytes;
use log::{error, info, trace, warn};
use scraper::{Html, Selector};
use std::sync::Arc;
use tokio::sync::{RwLock, RwLockWriteGuard};
use crate::anyhow_assert_eq;
use crate::client::post_bundle::live_post_bundle_feedback_manager::LivePostBundleFeedbackManager;
use crate::client::post_bundle::post_bundle_feedback_manager::PostBundleFeedbackManager;
use crate::client::timeline::multiple_timeline::MultipleTimeline;
use crate::protocol::payload::payload::{FetchUrlPreviewResponseV1, FetchUrlPreviewV1, PayloadRequestKind, PayloadResponseKind, PeerStatsRequestV1, PeerStatsResponseV1, SubmitPostCommitTokenV1, TrendingHashtagsFetchResponseV1, TrendingHashtagsFetchV1};
use crate::tools::compression;
use crate::tools::signing;
use crate::tools::types::VerificationKey;
use crate::protocol::peer::Peer;
use crate::protocol::rpc;
use crate::tools::config::CLIENT_FEEDBACK_POW_NUMERAIRE;
use crate::tools::plain_text_post::convert_text_to_hashiverse_html;
use crate::tools::tools;
use crate::tools::time::TimeMillis;

/// The top-level client API for participating in a hashiverse network.
///
/// `HashiverseClient` is what every external consumer — the server binary's cache role, the
/// WASM browser client, the Python wrapper, integration tests — builds on top of. It
/// orchestrates all the subsystems the client side needs:
///
/// - a [`PeerTracker`] for Kademlia-style peer discovery and gossip,
/// - a [`LivePostBundleManager`] / [`LivePostBundleFeedbackManager`] pair for fetching post
///   bundles and their feedback from the network (with on-disk caching),
/// - a [`SingleTimeline`] / [`MultipleTimeline`] pair for recursive bucket traversal when
///   reading a feed,
/// - a [`MetaPostManager`] for the user's profile / follow list / settings,
/// - a [`RecentPostsPen`] so freshly-authored posts appear in the user's own timelines
///   immediately,
/// - a [`ClientId`] / [`KeyLocker`] pair for the signed identity used on every outbound
///   action.
///
/// All external dependencies — clock, network, PoW — are carried through a single
/// [`RuntimeServices`], which is what makes the same code run inside the in-memory integration
/// test harness, on a native server, and in a browser Web Worker without touching any of the
/// high-level logic above.
///
/// Construct one via [`HashiverseClient::new`] and keep a single instance per logged-in
/// account; the struct is cheap to share (`Arc<Self>`) and its internal state uses `RwLock`s
/// for concurrent access.
pub struct HashiverseClient {
    runtime_services: Arc<RuntimeServices>,
    client_storage: Arc<dyn ClientStorage>,
    key_locker: Arc<dyn KeyLocker>,
    post_bundle_manager: Arc<LivePostBundleManager>,
    post_bundle_feedback_manager: Arc<LivePostBundleFeedbackManager>,
    meta_post_manager: MetaPostManager,
    client_id: ClientId,
    peer_tracker: Arc<RwLock<PeerTracker>>,
    recent_posts_pen: Arc<RwLock<RecentPostsPen>>,
    single_timeline: Arc<RwLock<Option<SingleTimeline>>>,
    multiple_timeline: Arc<RwLock<Option<MultipleTimeline>>>,
}

impl HashiverseClient {
    pub async fn new(runtime_services: Arc<RuntimeServices>, client_storage: Arc<dyn ClientStorage>, key_locker: Arc<dyn KeyLocker>, _args: Args) -> anyhow::Result<Self> {
        let client_id = key_locker.client_id().clone();
        info!("client_id={}", client_id);

        let peer_tracker = Arc::new(RwLock::new(PeerTracker::new(runtime_services.clone(), client_storage.clone()).await?));
        let post_bundle_manager = Arc::new(LivePostBundleManager::new(runtime_services.clone(), client_id.id, client_storage.clone(), peer_tracker.clone()));
        let post_bundle_feedback_manager = Arc::new(LivePostBundleFeedbackManager::new(runtime_services.clone(), client_id.id, client_storage.clone(), peer_tracker.clone()));

        // Trim the various buckets
        anyhow_assert_eq!(BUCKETS.len(), BUCKET_TRIMS.len(), "Mismatch in length between BUCKETS and BUCKET_TRIMS");
        info!("Trimming buckets: {} buckets, {} trims", BUCKETS.len(), BUCKET_TRIMS.len());
        for i in 0..BUCKETS.len() {
            let trim = BUCKET_TRIMS[i];
            if trim > 0 {
                client_storage.trim(BUCKETS[i], trim).await?;
            }
        }

        let meta_post_manager = MetaPostManager::new(runtime_services.clone(), client_storage.clone(), key_locker.clone(), client_id.clone());

        Ok(Self {
            runtime_services,
            client_storage,
            key_locker,
            post_bundle_manager,
            post_bundle_feedback_manager,
            meta_post_manager,
            client_id,
            peer_tracker,
            recent_posts_pen: Arc::new(RwLock::new(RecentPostsPen::new())),
            single_timeline: Arc::new(RwLock::new(None)),
            multiple_timeline: Arc::new(RwLock::new(None)),
        })
    }

    pub fn client_id(&self) -> &ClientId {
        &self.client_id
    }

    pub fn active_pow_jobs(&self) -> Vec<crate::tools::pow_generator::pow_generator::PowJobStatus> {
        self.runtime_services.pow_generator.active_jobs()
    }

    pub async fn client_storage_reset(&self) -> anyhow::Result<()> {
        self.client_storage.reset().await
    }

    pub async fn submit_post(&self, post: &str) -> Result<(Vec<SubmitPostCommitTokenV1>, (EncodedPostV1, Bytes)), anyhow::Error> {
        trace!("submitting post: {}", post);

        if post.is_empty() {
            anyhow::bail!("Post cannot be empty");
        }

        let timestamp = self.runtime_services.time_provider.current_time_millis();

        struct LinkedBaseIdDetail {
            linked_base_id: Id,
            bucket_type: BucketType,
            referenced_post_header_bytes: Option<Bytes>,
        }

        // Parse the post for #hashtags, @mentions, replies, sequels, etc.
        let mut linked_base_id_details: Vec<LinkedBaseIdDetail> = vec![];
        let mut referenced_hashtags: Vec<String> = vec![];

        // The original post itself is always present
        linked_base_id_details.push(LinkedBaseIdDetail { linked_base_id: self.client_id.id, bucket_type: BucketType::User, referenced_post_header_bytes: None });

        {
            let html = Html::parse_fragment(post);
            {
                // Returns true if the element is nested inside a <reply> or <repost> — meaning it
                // belongs to a quoted post and should not be indexed under the new post's buckets.
                let is_quoted = |element: scraper::ElementRef| -> bool {
                    let mut node = element.parent();
                    while let Some(n) = node {
                        if let Some(el) = scraper::ElementRef::wrap(n) {
                            if matches!(el.value().name(), "reply" | "repost" | "sequel") { return true; }
                        }
                        node = n.parent();
                    }
                    false
                };

                let selector_hashtag = Selector::parse("hashtag").map_err(|e| anyhow::anyhow!("Failed to parse hashtag selector: {}", e))?;
                for element in html.select(&selector_hashtag) {
                    if is_quoted(element) { continue; }
                    if let Some(hashtag) = element.attr("hashtag") {
                        trace!("hashtag={:?}", hashtag);
                        referenced_hashtags.push(hashtag.to_string());
                        linked_base_id_details.push(LinkedBaseIdDetail { linked_base_id: Id::from_hashtag_str(hashtag)?, bucket_type: BucketType::Hashtag, referenced_post_header_bytes: None });
                    } else {
                        warn!("hashtag attribute not found in element {:?}", element);
                    }
                }

                let selector_mention = Selector::parse("mention").map_err(|e| anyhow::anyhow!("Failed to parse mention selector: {}", e))?;
                for element in html.select(&selector_mention) {
                    if is_quoted(element) { continue; }
                    if let Some(client_id_str) = element.attr("client_id") {
                        match Id::from_hex_str(client_id_str) {
                            Ok(client_id) => {
                                trace!("mention_id={:?}", client_id);
                                linked_base_id_details.push(LinkedBaseIdDetail { linked_base_id: client_id, bucket_type: BucketType::Mention, referenced_post_header_bytes: None });
                            }
                            Err(e) => warn!("mention_id corrupted in element {:?}:, {}", element, e),
                        }
                    } else {
                        warn!("mention attribute not found in element {:?}", element);
                    }
                }

                let selector_reply = Selector::parse("reply").map_err(|e| anyhow::anyhow!("Failed to parse reply selector: {}", e))?;
                for element in html.select(&selector_reply) {
                    if is_quoted(element) { continue; }
                    if let Some(post_id_str) = element.attr("post_id") {
                        match Id::from_hex_str(post_id_str) {
                            Ok(post_id) => {
                                trace!("reply post_id={:?}", post_id);
                                let referenced_post_header_bytes = element.attr("post_header_hex")
                                    .and_then(|hex_str| hex::decode(hex_str).ok())
                                    .map(Bytes::from);
                                linked_base_id_details.push(LinkedBaseIdDetail { linked_base_id: post_id, bucket_type: BucketType::ReplyToPost, referenced_post_header_bytes });
                            }
                            Err(e) => warn!("reply post_id corrupted in element {:?}: {}", element, e),
                        }
                    } else {
                        warn!("post_id attribute not found in reply element {:?}", element);
                    }
                }

                let selector_sequel = Selector::parse("sequel").map_err(|e| anyhow::anyhow!("Failed to parse sequel selector: {}", e))?;
                for element in html.select(&selector_sequel) {
                    if is_quoted(element) { continue; }
                    if let Some(post_id_str) = element.attr("post_id") {
                        match Id::from_hex_str(post_id_str) {
                            Ok(post_id) => {
                                trace!("sequel post_id={:?}", post_id);
                                let referenced_post_header_bytes = element.attr("post_header_hex")
                                    .and_then(|hex_str| hex::decode(hex_str).ok())
                                    .map(Bytes::from);
                                linked_base_id_details.push(LinkedBaseIdDetail { linked_base_id: post_id, bucket_type: BucketType::Sequel, referenced_post_header_bytes });
                            }
                            Err(e) => warn!("sequel post_id corrupted in element {:?}: {}", element, e),
                        }
                    } else {
                        warn!("post_id attribute not found in sequel element {:?}", element);
                    }
                }
            }
        }

        // Encode the post
        let linked_base_ids: Vec<Id> = linked_base_id_details.iter().map(|d| d.linked_base_id).collect();
        let mut encoded_post = EncodedPostV1::new(&self.client_id, timestamp, linked_base_ids, post);
        let encoded_post_bytes = encoded_post.encode_to_bytes_direct(&self.key_locker).await?;

        // The commit tokens for the User's post
        let mut post_commit_tokens = Vec::new();

        // Start the post machine
        for linked_base_id_detail in &linked_base_id_details {
            trace!("Posting to bucket type: {:?}, linked_base_id: {}", linked_base_id_detail.bucket_type, linked_base_id_detail.linked_base_id);
            let try_result = try {
                // Where are we going to post it?  Start at the widest bucket and work our way narrower till we succeed
                for &bucket_duration in bucket_durations_for_type(linked_base_id_detail.bucket_type) {
                    let bucket_location = generate_bucket_location(linked_base_id_detail.bucket_type, linked_base_id_detail.linked_base_id, bucket_duration, timestamp)?;
                    info!("checking posting availability of {:?}", bucket_location);

                    let post_bundle = self.post_bundle_manager.get_post_bundle(&bucket_location, timestamp).await?;
                    if !post_bundle.header.overflowed && !post_bundle.header.sealed {
                        info!("Posting to {:?}", bucket_location);
                        let result = posting::post_to_location(&self.runtime_services, &self.client_id.id, &self.peer_tracker, &bucket_location, &encoded_post, &encoded_post_bytes, linked_base_id_detail.referenced_post_header_bytes.as_deref(), &referenced_hashtags).await;
                        match result {
                            Ok(mut result) => {
                                post_commit_tokens.append(&mut result);
                                break;
                            }
                            Err(e) => {
                                warn!("Failed to post to {:?}: {}", bucket_location, e);
                                continue;
                            }
                        }
                    }

                    else {
                        trace!("no availability: overflowed={} sealed={}", post_bundle.header.overflowed, post_bundle.header.sealed);
                    }
                }
            };

            if let Err(e) = try_result {
                warn!("Failed to post to any bucket location for linked_base_id {:?}: {}", linked_base_id_detail.linked_base_id, e);
            }

            // Check that we managed at least one commit by the end of the User bucket (healing should fix the rest).  We can happily continue with a failed hashtag, mention, etc.
            if linked_base_id_detail.bucket_type == BucketType::User && post_commit_tokens.is_empty() {
                anyhow::bail!("Failed to post to any User buckets, so bailing,");
            }
        }

        let encoded_post_bytes_raw = Bytes::copy_from_slice(encoded_post_bytes.bytes());

        // Record in the recent posts pen so these posts appear immediately in timeline fetches
        {
            let bucket_locations_and_post_ids: Vec<_> = post_commit_tokens.iter()
                .map(|token| (token.bucket_location.clone(), token.post_id))
                .collect();
            self.recent_posts_pen.write().await.add_all(&bucket_locations_and_post_ids, encoded_post_bytes_raw.clone(), timestamp);
        }

        Ok((post_commit_tokens, (encoded_post, encoded_post_bytes_raw)))
    }

    // ------------------------------------------------------------------
    // MetaPostManager delegation
    // ------------------------------------------------------------------

    pub fn meta_post_manager(&self) -> &MetaPostManager {
        &self.meta_post_manager
    }

    pub async fn submit_meta_post(&self) -> anyhow::Result<()> {
        let post_json = self.meta_post_manager.build_meta_post_json().await?;
        self.submit_post(&post_json).await?;
        Ok(())
    }

    pub async fn ensure_meta_post_in_current_bucket(&self) -> anyhow::Result<()> {
        if self.meta_post_manager.should_auto_publish(self.post_bundle_manager.as_ref()).await? {
            self.submit_meta_post().await?;
        }
        Ok(())
    }

    pub async fn submit_feedback(&self, bucket_location: BucketLocation, post_id: Id, feedback_type: u8) -> anyhow::Result<()> {
        info!("submit_feedback: bucket_location={}, post_signature={}, feedback_type={}", bucket_location, post_id, feedback_type);

        // Generate our PoW
        let (salt, pow, _hash) = EncodedPostFeedbackV1::pow_generate(&post_id, feedback_type, self.runtime_services.pow_generator.as_ref()).await?;

        // Is it better than the best we have seen so far?
        let post_bundle_location_id = bucket_location.location_id;
        let post_bundle_feedback = self.post_bundle_feedback_manager.get_post_bundle_feedback(bucket_location.clone(), self.runtime_services.time_provider.current_time_millis()).await?;
        let pow_best_so_far = post_bundle_feedback.get_post_pow_for_feedback_type(&post_id, feedback_type);
        if pow <= pow_best_so_far {
            trace!("skipping feedback submission: pow_best_so_far: {}, pow: {}", pow_best_so_far, pow);
            return Ok(());
        }

        // Submit our good work to the servers
        let encoded_post_feedback = EncodedPostFeedbackV1::new(post_id, feedback_type, salt, pow);
        let result = posting::post_feedback_to_location(
            &self.runtime_services, &self.client_id.id, &self.peer_tracker,
            &bucket_location, &encoded_post_feedback,
        ).await;
        if let Err(e) = result {
            warn!("Failed to feedback to {:?}: {}", post_bundle_location_id, e);
        }

        Ok(())
    }

    pub async fn get_post(&self, bucket_location: BucketLocation, post_id: &Id) -> anyhow::Result<(BucketLocation, EncodedPostV1, Bytes, bool)>
    {
        let post_bundle = self.post_bundle_manager.get_post_bundle(&bucket_location, self.runtime_services.time_provider.current_time_millis()).await?;

        let mut offset = 0;
        for i in 0..(post_bundle.header.num_posts as usize) {
            let len = post_bundle.header.encoded_post_lengths[i];
            if post_bundle.header.encoded_post_ids[i] == *post_id {
                let post_bytes = post_bundle.encoded_posts_bytes.slice(offset..offset + len);
                let encoded_post = EncodedPostV1::decode_from_bytes(post_bytes.clone(), &bucket_location.base_id, true, true)?;
                let healed = post_bundle.header.encoded_post_healed.contains(post_id);
                return Ok((bucket_location, encoded_post, post_bytes, healed));
            }
            offset += len;
        }

        anyhow::bail!("Post {} not found in bundle {}", post_id, bucket_location.location_id)
    }

    pub async fn get_post_feedbacks(&self, bucket_location: BucketLocation, post_id: Id) -> anyhow::Result<[u64; 256]>
    {
        let mut post_feedbacks = [0u64; 256];

        let post_bundle_feedback = self.post_bundle_feedback_manager.get_post_bundle_feedback(bucket_location, self.runtime_services.time_provider.current_time_millis()).await?;
        let post_pows = post_bundle_feedback.get_post_pows(&post_id);
        for (i, &pow) in post_pows.iter().enumerate() {
            if pow.0 > 0 {
                let statistical_attempts = 1u64.checked_shl(pow.0 as u32).unwrap_or(u64::MAX);
                post_feedbacks[i] = statistical_attempts / CLIENT_FEEDBACK_POW_NUMERAIRE as u64;
            }
            // pow=0 is the sentinel for "no feedback submitted" — leave as 0
        }

        Ok(post_feedbacks)
    }


    async fn post_process_timeline_posts(&self, encoded_posts_bytes: Vec<(BucketLocation, Bytes, bool)>) -> anyhow::Result<Vec<(BucketLocation, EncodedPostV1, Bytes, bool)>> {
        let mut encoded_posts = Vec::new();
        for (bucket_location, encoded_post_bytes, healed) in encoded_posts_bytes {
            let result = try {
                let encoded_post = EncodedPostV1::decode_from_bytes(encoded_post_bytes.clone(), &bucket_location.base_id, true, true)?;
                let meta_post = MetaPost::try_parse_meta_post(&encoded_post.post)?;
                match meta_post {
                    MetaPost::None => encoded_posts.push((bucket_location, encoded_post, encoded_post_bytes, healed)),
                    MetaPost::MetaPostV1(meta_post_v1) => {
                        let post_client_id = encoded_post.header.client_id()?;
                        self.meta_post_manager.process_incoming_meta_post(&meta_post_v1, &post_client_id).await?;
                    }
                }
            };

            if let Err(e) = result {
                warn!("Failed to decode post: {}", e);
            }
        }

        Ok(encoded_posts)
    }

    pub async fn single_timeline_reset(&self) -> anyhow::Result<()> {
        info!("Resetting single timeline");
        let mut single_timeline = self.single_timeline.write().await;
        *single_timeline = None;
        Ok(())
    }

    pub async fn single_timeline_lock(&self, bucket_type: BucketType, base_id: &Id) -> anyhow::Result<RwLockWriteGuard<'_, Option<SingleTimeline>>> {
        let mut single_timeline = self.single_timeline.write().await;

        // If our base_id has changed, blow it away
        if let Some(single_timeline_instance) = single_timeline.as_ref() {
            if single_timeline_instance.bucket_type() != bucket_type || single_timeline_instance.base_id() != *base_id {
                *single_timeline = None;
            }
        }

        // Create the single_timeline if necessary
        if single_timeline.is_none() {
            trace!("Starting a new SingleTimeline for bucket_type={} base_id={}", bucket_type, base_id);
            *single_timeline = Some(SingleTimeline::new(bucket_type, base_id, self.post_bundle_manager.clone(), self.recent_posts_pen.clone()));
        }

        Ok(single_timeline)
    }

    pub async fn single_timeline_get_more(&self, bucket_type: BucketType, base_id: &Id) -> anyhow::Result<(Vec<(BucketLocation, EncodedPostV1, Bytes, bool)>, TimeMillis)> {
        trace!("Getting more posts for {}", base_id);

        let mut single_timeline = self.single_timeline_lock(bucket_type, base_id).await?;
        let single_timeline = single_timeline.as_mut().expect("we have ensured that our SingleTimeline exists");
        let encoded_posts_bytes = single_timeline.get_more_posts(self.runtime_services.time_provider.current_time_millis(), 20, bucket_durations_for_type(bucket_type)).await?;
        let oldest_processed_time_millis = single_timeline.oldest_processed_post_bundle_time_millis();
        let posts = self.post_process_timeline_posts(encoded_posts_bytes).await?;
        Ok((posts, oldest_processed_time_millis))
    }

    pub async fn multiple_timeline_reset(&self) -> anyhow::Result<()> {
        info!("Resetting multiple timeline");
        let mut multiple_timeline = self.multiple_timeline.write().await;
        *multiple_timeline = None;
        Ok(())
    }

    pub async fn multiple_timeline_lock(&self, bucket_type: BucketType, base_ids: &Vec<Id>) -> anyhow::Result<RwLockWriteGuard<'_, Option<MultipleTimeline>>> {
        let mut multiple_timeline = self.multiple_timeline.write().await;

        // If our base_ids have changed, blow it away
        if let Some(multiple_timeline_instance) = multiple_timeline.as_ref() {
            if multiple_timeline_instance.bucket_type() != bucket_type || multiple_timeline_instance.base_ids() != base_ids {
                *multiple_timeline = None;
            }
        }

        // Create the multiple_timeline if necessary
        if multiple_timeline.is_none() {
            trace!("Starting a new MultipleTimeline for base_ids.len()={}", base_ids.len());
            *multiple_timeline = Some(MultipleTimeline::new(bucket_type, base_ids.clone(), self.post_bundle_manager.clone(), self.recent_posts_pen.clone()));
        }

        Ok(multiple_timeline)
    }

    pub async fn multiple_timeline_get_more(&self, bucket_type: BucketType, base_ids: &Vec<Id>) -> anyhow::Result<(Vec<(BucketLocation, EncodedPostV1, Bytes, bool)>, TimeMillis)> {
        trace!("Getting more posts for base_ids.len()={}", base_ids.len());

        let mut multiple_timeline = self.multiple_timeline_lock(bucket_type, base_ids).await?;
        let multiple_timeline = multiple_timeline.as_mut().expect("we have ensured that our MultipleTimeline exists");
        let encoded_posts_bytes = multiple_timeline.get_more_posts(self.runtime_services.time_provider.current_time_millis(), 60, 5, bucket_durations_for_type(bucket_type)).await?;
        let oldest_processed_time_millis = multiple_timeline.oldest_processed_post_bundle_time_millis();
        let posts = self.post_process_timeline_posts(encoded_posts_bytes).await?;
        Ok((posts, oldest_processed_time_millis))
    }

    async fn get_random_peer(&self) -> anyhow::Result<Peer> {
        {
            let peer_tracker = self.peer_tracker.read().await;
            if !peer_tracker.peers().is_empty() {
                return Ok(tools::random_element(peer_tracker.peers()).clone());
            }
        }

        // No peers — try bootstrapping
        {
            let mut peer_tracker = self.peer_tracker.write().await;
            peer_tracker.bootstrap().await?;
            anyhow::ensure!(!peer_tracker.peers().is_empty(), "Still no known peers available after bootstrap");
            Ok(tools::random_element(peer_tracker.peers()).clone())
        }
    }

    pub async fn get_all_known_peers(&self) -> Vec<Peer> {
        self.peer_tracker.read().await.peers().clone()
    }

    pub async fn fetch_url_preview(&self, url: &str) -> anyhow::Result<FetchUrlPreviewResponseV1> {
        let peer = self.get_random_peer().await?;

        let payload = FetchUrlPreviewV1::new_to_bytes(url)?;
        let sponsor_id = self.client_id.id;

        let response = rpc::rpc::rpc_server_known_with_requisite_pow(
            &self.runtime_services,
            &sponsor_id,
            &peer,
            PayloadRequestKind::FetchUrlPreviewV1,
            payload,
            crate::tools::config::POW_MINIMUM_PER_URL_FETCH,
        ).await?;

        anyhow::ensure!(response.response_request_kind == PayloadResponseKind::FetchUrlPreviewResponseV1, "unexpected response kind: {}", response.response_request_kind);
        FetchUrlPreviewResponseV1::from_bytes(&response.bytes)
    }

    /// Issue a `PeerStatsRequestV1` to the peer identified by `peer_id` and return
    /// the verified, decompressed stats document.
    ///
    /// The signature is verified against the response's own `peer` field — callers
    /// receive either a verified `serde_json::Value` or an error. The doc's schema
    /// is open-ended; the server decides what fields to include and at what nesting.
    pub async fn fetch_peer_stats(&self, peer_id: &Id) -> anyhow::Result<serde_json::Value> {
        let peer = {
            let peer_tracker = self.peer_tracker.read().await;
            peer_tracker.peers().iter().find(|peer| &peer.id == peer_id).cloned()
        };
        let peer = peer.ok_or_else(|| anyhow::anyhow!("peer not known to this client: {}", peer_id))?;

        let payload = PeerStatsRequestV1 {}.to_bytes()?;
        let sponsor_id = self.client_id.id;

        let response = rpc::rpc::rpc_server_known_with_requisite_pow(
            &self.runtime_services,
            &sponsor_id,
            &peer,
            PayloadRequestKind::PeerStatsRequestV1,
            payload,
            crate::tools::config::POW_MINIMUM_PER_PEER_STATS,
        ).await?;

        anyhow::ensure!(response.response_request_kind == PayloadResponseKind::PeerStatsResponseV1, "unexpected response kind: {}", response.response_request_kind);
        let response = PeerStatsResponseV1::from_bytes(&response.bytes)?;

        let verification_key = VerificationKey::from_bytes(&response.peer.verification_key_bytes)?;
        let signing_input = PeerStatsResponseV1::signing_input(response.timestamp, &response.json_compressed);
        signing::verify(&verification_key, &response.signature, &signing_input)?;

        let json_bytes = compression::decompress(&response.json_compressed)?.to_bytes();
        let doc: serde_json::Value = serde_json::from_slice(&json_bytes)?;
        Ok(doc)
    }

    pub async fn fetch_trending_hashtags(&self, limit: u16) -> anyhow::Result<TrendingHashtagsFetchResponseV1> {
        let peer = self.get_random_peer().await?;

        let payload = TrendingHashtagsFetchV1::new_to_bytes(limit)?;
        let sponsor_id = self.client_id.id;

        let response = rpc::rpc::rpc_server_known(
            &self.runtime_services,
            &sponsor_id,
            &peer,
            PayloadRequestKind::TrendingHashtagsFetchV1,
            payload,
        ).await?;

        anyhow::ensure!(response.response_request_kind == PayloadResponseKind::TrendingHashtagsFetchResponseV1, "unexpected response kind: {}", response.response_request_kind);
        TrendingHashtagsFetchResponseV1::from_bytes(&response.bytes)
    }

    pub async fn dispatch_command(&self, command: &String) -> Result<(), anyhow::Error> {
        let mut command_parts: Vec<String> = command.splitn(2, " ").map(|s| s.to_string()).collect();
        if command_parts.is_empty() {
            anyhow::bail!("Command cannot be empty")
        }

        command_parts[0] = command_parts[0].to_uppercase();

        match command_parts[0].as_str() {
            // ID
            "I" => {
                info!("I am hashiverse_client {}", self.client_id);
            }

            // POST
            "P" => {
                if command_parts.len() < 2 {
                    anyhow::bail!("Post message cannot be empty")
                }
                let post_html = convert_text_to_hashiverse_html(&command_parts[1]);
                let payload_response = self.submit_post(&post_html).await;
                match payload_response {
                    Ok(_) => {
                        info!("post succeeded");
                    }
                    Err(e) => {
                        error!("post error: {}", e);
                    }
                }
            }

            // MY POSTS
            "M" => {
                let encoded_posts = self.single_timeline_get_more(BucketType::User, &self.client_id.id).await;
                match encoded_posts {
                    Ok((encoded_posts, _oldest_processed_time_millis)) => {
                        info!("received {} more posts", encoded_posts.len());
                        for (bucket_location_id, encoded_post, _raw_bytes, _healed) in encoded_posts {
                            info!("post: {} {} {}", bucket_location_id, encoded_post.header.time_millis, encoded_post.post);
                        }
                    }
                    Err(e) => {
                        error!("post error: {}", e);
                    }
                }
            }

            _ => {
                warn!("unknown command: {}", command);
            }
        }

        Ok(())
    }
}

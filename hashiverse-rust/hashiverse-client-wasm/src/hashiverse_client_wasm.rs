use crate::wasm_client_storage::WasmClientStorage;
use crate::wasm_key_locker::WasmKeyLockerManager;
use crate::wasm_transport::WasmTransportFactory;
use crate::wasm_try;
use hashiverse_lib::client::args::Args;
use hashiverse_lib::client::hashiverse_client::HashiverseClient;
use hashiverse_lib::client::key_locker::key_locker::KeyLockerManager;
use hashiverse_lib::tools::buckets::{BucketLocation, BucketType};
use hashiverse_lib::tools::time::TimeMillis;
use hashiverse_lib::tools::time_provider::time_provider::RealTimeProvider;
use hashiverse_lib::tools::pow_generator::pow_generator::PowGenerator;
use hashiverse_lib::tools::pow_generator::single_threaded_pow_generator::SingleThreadedPowGenerator;
use hashiverse_lib::tools::runtime_services::RuntimeServices;
use hashiverse_lib::tools::types::Id;
use log::warn;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use anyhow::anyhow;
use tsify::Tsify;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use bytes::Bytes;
use hashiverse_lib::protocol::posting::encoded_post::EncodedPostV1;

#[wasm_bindgen]
/// Provides a simplified dispatch interface for [HashiverseClient] to the browser.
pub struct HashiverseClientWasm {
    logged_in: bool,
    hashiverse_client: HashiverseClient,
}

#[wasm_bindgen]
impl HashiverseClientWasm {
    async fn create_from_xxx(logged_in: bool, key_locker: Arc<dyn hashiverse_lib::client::key_locker::key_locker::KeyLocker>) -> anyhow::Result<Self> {
        let time_provider: Arc<dyn hashiverse_lib::tools::time_provider::time_provider::TimeProvider> = Arc::new(RealTimeProvider::default());
        let transport_factory: Arc<dyn hashiverse_lib::transport::transport::TransportFactory> = Arc::new(WasmTransportFactory::default());
        let client_storage = WasmClientStorage::new().await?;
        let pow_generator: Arc<dyn PowGenerator> = match crate::get_wasm_parallel_pow_generator() {
            Some(g) => g as Arc<dyn PowGenerator>,
            None => {
                warn!("No native PoW generator available, falling back to SingleThreadedPowGenerator");
                Arc::new(SingleThreadedPowGenerator::new())
            }
        };
        let runtime_services = Arc::new(RuntimeServices { time_provider, transport_factory, pow_generator });
        let hashiverse_client = HashiverseClient::new(runtime_services, client_storage, key_locker, Args::new()).await?;
        Ok(Self { logged_in, hashiverse_client })
    }

    #[wasm_bindgen]
    pub async fn create_from_keyphrase(key_phrase: String) -> Result<Self, JsValue> {
        wasm_try!({
            let logged_in = !key_phrase.is_empty();
            let key_locker_manager = WasmKeyLockerManager::new().await?;
            let key_locker = key_locker_manager.create(key_phrase).await?;
            Self::create_from_xxx(logged_in, key_locker).await?
        })
    }

    #[wasm_bindgen]
    pub async fn create_from_stored_key(client_id_hex: String) -> Result<Self, JsValue> {
        wasm_try!({
            let key_locker_manager = WasmKeyLockerManager::new().await?;
            let key_locker = key_locker_manager.switch(client_id_hex).await?;
            Self::create_from_xxx(true, key_locker).await?
        })
    }

    #[wasm_bindgen]
    pub fn logged_in(&self) -> bool {
        self.logged_in
    }

    #[wasm_bindgen]
    pub async fn list_stored_key_ids_v1(&self) -> Result<Vec<String>, JsValue> {
        wasm_try!({
            let key_locker_manager = WasmKeyLockerManager::new().await?;
            key_locker_manager.list().await?
        })
    }

    #[wasm_bindgen]
    pub async fn delete_stored_key_v1(&self, key_public: String) -> Result<(), JsValue> {
        wasm_try!({
            let key_locker_manager = WasmKeyLockerManager::new().await?;
            key_locker_manager.delete(key_public).await?;
        })
    }

    #[wasm_bindgen]
    pub async fn delete_all_stored_keys_v1(&self) -> Result<(), JsValue> {
        wasm_try!({
            let key_locker_manager = WasmKeyLockerManager::new().await?;
            key_locker_manager.reset().await?;
        })
    }

    #[wasm_bindgen]
    pub fn get_client_id(&self) -> String {
        self.hashiverse_client.client_id().id_hex()
    }

    #[wasm_bindgen]
    pub async fn client_storage_reset(&self) -> Result<(), JsValue> {
        wasm_try!({
            self.hashiverse_client.client_storage_reset().await?;
        })
    }

    #[wasm_bindgen]
    pub async fn post_v1(&self, post: &str) -> Result<Post, JsValue> {
        wasm_try!({
            let (commit_tokens, (encoded_post, raw_bytes)) = self.hashiverse_client.submit_post(post).await?;
            let bucket_location = &commit_tokens[0].bucket_location;
            let client_id = encoded_post.header.client_id()?;
            let encoded_post_header_hex = hex::encode(EncodedPostV1::bytes_without_body(raw_bytes)?);
            Post {
                post_id: encoded_post.post_id.to_hex_str(),
                time_millis: encoded_post.header.time_millis.0,
                client_id: client_id.id_hex(),
                bucket_location: bucket_location.to_html_attr(),
                post: encoded_post.post,
                encoded_post_header_hex,
                healed: false,
            }
        })
    }

    fn meta_post_manager(&self) -> &hashiverse_lib::client::meta_post::meta_post_manager::MetaPostManager {
        self.hashiverse_client.meta_post_manager()
    }

    pub async fn set_bio(&self, nickname: String, status: String, selfie: String, avatar: String) -> Result<(), JsValue> {
        wasm_try!({
            self.meta_post_manager().set_bio(nickname, status, selfie, avatar).await?;
        })
    }

    #[wasm_bindgen]
    pub async fn submit_feedback_v1(&self, bucket_location: String, post_id: String, feedback_type: u8) -> Result<(), JsValue> {
        wasm_try!({
            let bucket_location = BucketLocation::from_html_attr(&bucket_location)?;
            let post_id = Id::from_hex_str(&post_id)?;
            self.hashiverse_client.submit_feedback(bucket_location, post_id, feedback_type).await?;
        })
    }

    #[wasm_bindgen]
    /// Gets a specific post
    pub async fn get_post_v1(&self, bucket_location: String, post_id: String) -> Result<Post, JsValue> {
        wasm_try!({
            let bucket_location = BucketLocation::from_html_attr(&bucket_location)?;
            let post_id = Id::from_hex_str(&post_id)?;
            let (bucket_location, post, raw_bytes, healed) = self.hashiverse_client.get_post(bucket_location, &post_id).await?;
            let client_id = post.header.client_id()?;
            let encoded_post_header_hex = hex::encode(EncodedPostV1::bytes_without_body(raw_bytes)?);
            Post {
                post_id: post.post_id.to_hex_str(),
                time_millis: post.header.time_millis.0,
                client_id: client_id.id_hex(),
                bucket_location: bucket_location.to_html_attr(),
                post: post.post,
                encoded_post_header_hex,
                healed,
            }
        })
    }

    #[wasm_bindgen]
    /// Gets all the feedbacks for a specific post
    ///
    /// The resulting vector has 256 entries - one per feedback_type that have been mapped to the statistical number of clicks a feedback button has received.
    pub async fn get_post_feedbacks_v1(&self, bucket_location: String, post_id: String) -> Result<Vec<u32>, JsValue> {
        wasm_try!({
            let bucket_location = BucketLocation::from_html_attr(&bucket_location)?;
            let post_id = Id::from_hex_str(&post_id)?;
            let post_feedbacks = self.hashiverse_client.get_post_feedbacks(bucket_location, post_id).await?;
            post_feedbacks.iter().map(|&feedback| feedback.min(u32::MAX as u64) as u32).collect()
        })
    }

    #[wasm_bindgen]
    pub async fn get_bio(&self, id: String) -> Result<Bio, JsValue> {
        wasm_try!({
            let meta_post_public = self.meta_post_manager().get_meta_post_public(Id::from_hex_str(&id)?).await?;
            match meta_post_public {
                Some(meta_post_public) => Bio {
                    client_id: id,
                    nickname: meta_post_public.nickname.value.unwrap_or_default(),
                    status: meta_post_public.status.value.unwrap_or_default(),
                    selfie: meta_post_public.selfie.value.unwrap_or_default(),
                    avatar: meta_post_public.avatar.value.unwrap_or_default(),
                },
                None => Bio {
                    client_id: id,
                    nickname: "".to_string(),
                    status: "".to_string(),
                    selfie: "".to_string(),
                    avatar: "".to_string(),
                },
            }
        })
    }

    #[wasm_bindgen]
    pub async fn get_all_bios(&self) -> Result<Vec<Bio>, JsValue> {
        wasm_try!({
            let meta_post_publics = self.meta_post_manager().get_all_meta_post_publics().await?;
            meta_post_publics.into_iter()
                .map(|(client_id, meta_post_public)| Bio {
                    client_id,
                    nickname: meta_post_public.nickname.value.unwrap_or_default(),
                    status: meta_post_public.status.value.unwrap_or_default(),
                    selfie: meta_post_public.selfie.value.unwrap_or_default(),
                    avatar: meta_post_public.avatar.value.unwrap_or_default(),
                })
                .collect()
        })
    }

    #[wasm_bindgen]
    pub async fn get_all_known_peers_v1(&self) -> Result<Vec<PeerInfoV1>, JsValue> {
        wasm_try!({
            self.hashiverse_client.get_all_known_peers().await
                .into_iter()
                .map(|peer| PeerInfoV1 {
                    peer_id_hex: peer.id.to_hex_str(),
                    address: peer.address,
                    version: peer.version,
                    timestamp_millis: peer.timestamp.0,
                    pow_initial: peer.pow_initial.pow.0,
                    pow_current_day: peer.pow_current_day.pow.0,
                    pow_current_month: peer.pow_current_month.pow.0,
                })
                .collect::<Vec<_>>()
        })
    }

    #[wasm_bindgen]
    pub async fn get_peer_stats_v1(&self, peer_id_hex: String) -> Result<JsValue, JsValue> {
        wasm_try!({
            let peer_id = Id::from_hex_str(&peer_id_hex)?;
            let doc = self.hashiverse_client.fetch_peer_stats(&peer_id).await?;
            let serializer = serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true);
            doc.serialize(&serializer).map_err(|e| anyhow!("serde_wasm_bindgen error: {}", e))?
        })
    }

    #[wasm_bindgen]
    pub async fn get_active_pow_jobs_v1(&self) -> Result<Vec<PowJobStatusV1>, JsValue> {
        wasm_try!({
            self.hashiverse_client.active_pow_jobs()
                .into_iter()
                .map(|job| PowJobStatusV1 {
                    label: job.label,
                    pow_min: job.pow_min.0,
                    best_pow_so_far: job.best_pow_so_far.0,
                })
                .collect::<Vec<_>>()
        })
    }

    /// Whether there is background PoW work happening now, or within the last `within_millis`.
    /// Polled by the GUI to show/hide the "busy" indicator.
    #[wasm_bindgen]
    pub async fn is_pow_busy_v1(&self, within_millis: u32) -> Result<bool, JsValue> {
        wasm_try!({ self.hashiverse_client.is_pow_busy(within_millis as i64) })
    }

    fn post_process_timeline_posts(&self, encoded_posts: Vec<(BucketLocation, EncodedPostV1, Bytes, bool)>, oldest_processed_time_millis: TimeMillis) -> anyhow::Result<SingleTimelineGetMoreV1Response> {
        let response = SingleTimelineGetMoreV1Response {
            oldest_processed_time_millis: if oldest_processed_time_millis == TimeMillis::MAX { None } else { Some(oldest_processed_time_millis.0) },
            posts: encoded_posts
                .into_iter()
                .filter_map(|(bucket_location, post, raw_bytes, healed)| {
                    let client_id = match post.header.client_id() {
                        Ok(client_id) => client_id,
                        Err(e) => {
                            warn!("Skipping post with bad client_id in header: {}", e);
                            return None;
                        }
                    };
                    let encoded_post_header_hex = match EncodedPostV1::bytes_without_body(raw_bytes) {
                        Ok(header_bytes) => hex::encode(header_bytes),
                        Err(e) => {
                            warn!("Skipping post with bad header bytes: {}", e);
                            return None;
                        }
                    };
                    Some(Post {
                        post_id: post.post_id.to_hex_str(),
                        time_millis: post.header.time_millis.0,
                        client_id: client_id.id_hex(),
                        bucket_location: bucket_location.to_html_attr(),
                        post: post.post,
                        encoded_post_header_hex,
                        healed,
                    })
                })
                .collect(),
        };

        Ok(response)
    }


    #[wasm_bindgen]
    pub async fn single_timeline_reset(&self) -> Result<(), JsValue> {
        wasm_try!({
            self.hashiverse_client.single_timeline_reset().await?;
        })
    }

    async fn single_timeline_get_more(&self, bucket_type: BucketType, base_id: &Id) -> anyhow::Result<SingleTimelineGetMoreV1Response> {
        let (encoded_posts, oldest_processed_time_millis) = self.hashiverse_client.single_timeline_get_more(bucket_type, base_id).await?;
        self.post_process_timeline_posts(encoded_posts, oldest_processed_time_millis)
    }

    #[wasm_bindgen]
    pub async fn single_timeline_get_more_me_v1(&self) -> Result<SingleTimelineGetMoreV1Response, JsValue> {
        wasm_try!({
            let id = self.hashiverse_client.client_id().id;
            self.single_timeline_get_more(BucketType::User, &id).await?
        })
    }

    #[wasm_bindgen]
    pub async fn single_timeline_get_more_me_mentioned_v1(&self) -> Result<SingleTimelineGetMoreV1Response, JsValue> {
        wasm_try!({
            let id = self.hashiverse_client.client_id().id;
            self.single_timeline_get_more(BucketType::Mention, &id).await?
        })
    }

    #[wasm_bindgen]
    pub async fn single_timeline_get_more_hashtag_v1(&self, hashtag: String) -> Result<SingleTimelineGetMoreV1Response, JsValue> {
        wasm_try!({
            let id = Id::from_hashtag_str(&hashtag)?;
            self.single_timeline_get_more(BucketType::Hashtag, &id).await?
        })
    }

    #[wasm_bindgen]
    pub async fn single_timeline_get_more_user_v1(&self, client_id_hex: String) -> Result<SingleTimelineGetMoreV1Response, JsValue> {
        wasm_try!({
            let id = Id::from_hex_str(&client_id_hex)?;
            self.single_timeline_get_more(BucketType::User, &id).await?
        })
    }

    #[wasm_bindgen]
    pub async fn single_timeline_get_more_user_mentioned_v1(&self, client_id_hex: String) -> Result<SingleTimelineGetMoreV1Response, JsValue> {
        wasm_try!({
            let id = Id::from_hex_str(&client_id_hex)?;
            self.single_timeline_get_more(BucketType::Mention, &id).await?
        })
    }

    #[wasm_bindgen]
    pub async fn single_timeline_get_more_reply_to_post_v1(&self, post_id: String) -> Result<SingleTimelineGetMoreV1Response, JsValue> {
        wasm_try!({
            let id = Id::from_hex_str(&post_id)?;
            self.single_timeline_get_more(BucketType::ReplyToPost, &id).await?
        })
    }

    #[wasm_bindgen]
    pub async fn single_timeline_get_more_sequel_v1(&self, post_id: String) -> Result<SingleTimelineGetMoreV1Response, JsValue> {
        wasm_try!({
            let id = Id::from_hex_str(&post_id)?;
            self.single_timeline_get_more(BucketType::Sequel, &id).await?
        })
    }

    #[wasm_bindgen]
    pub async fn multiple_timeline_reset(&self) -> Result<(), JsValue> {
        wasm_try!({
            self.hashiverse_client.multiple_timeline_reset().await?;
        })
    }

    async fn multiple_timeline_get_more(&self, bucket_type: BucketType, base_ids: &Vec<Id>) -> anyhow::Result<SingleTimelineGetMoreV1Response> {
        let (encoded_posts, oldest_processed_time_millis) = self.hashiverse_client.multiple_timeline_get_more(bucket_type, base_ids).await?;
        self.post_process_timeline_posts(encoded_posts, oldest_processed_time_millis)
    }

    #[wasm_bindgen]
    pub async fn multiple_timeline_get_more_followed_users(&self) -> Result<SingleTimelineGetMoreV1Response, JsValue> {
        wasm_try!({
            let ids = self.meta_post_manager().get_followed_client_ids().await?;
            self.multiple_timeline_get_more(BucketType::User, &ids).await?
        })
    }

    #[wasm_bindgen]
    pub async fn get_followed_client_ids_v1(&self) -> Result<Vec<String>, JsValue> {
        wasm_try!({
            let ids = self.meta_post_manager().get_followed_client_ids().await?;
            ids.into_iter().map(|id| id.to_hex_str()).collect()
        })
    }

    #[wasm_bindgen]
    pub async fn set_followed_client_ids_v1(&self, client_ids: JsValue) -> Result<(), JsValue> {
        wasm_try!({
            let client_id_strs: Vec<String> = serde_wasm_bindgen::from_value(client_ids).map_err(|e| anyhow!("serde_wasm_bindgen::from_value error: {}", e))?;
            let ids = client_id_strs.iter().map(|s| Id::from_hex_str(s)).collect::<anyhow::Result<Vec<_>>>()?;
            self.meta_post_manager().set_followed_client_ids(ids).await?;
        })
    }

    #[wasm_bindgen]
    pub async fn set_followed_client_id_v1(&self, client_id: String, is_followed: bool) -> Result<(), JsValue> {
        wasm_try!({
            let id = Id::from_hex_str(&client_id)?;
            self.meta_post_manager().set_followed_client_id(id, is_followed).await?;
        })
    }

    #[wasm_bindgen]
    pub async fn multiple_timeline_get_more_followed_hashtags(&self) -> Result<SingleTimelineGetMoreV1Response, JsValue> {
        wasm_try!({
            let hashtags = self.meta_post_manager().get_followed_hashtags().await?;
            let ids = hashtags.iter().map(|h| Id::from_hashtag_str(h)).collect::<anyhow::Result<Vec<_>>>()?;
            self.multiple_timeline_get_more(BucketType::Hashtag, &ids).await?
        })
    }

    #[wasm_bindgen]
    pub async fn get_followed_hashtags_v1(&self) -> Result<Vec<String>, JsValue> {
        wasm_try!({
            self.meta_post_manager().get_followed_hashtags().await?
        })
    }

    #[wasm_bindgen]
    pub async fn set_followed_hashtags_v1(&self, hashtags: JsValue) -> Result<(), JsValue> {
        wasm_try!({
            let hashtags: Vec<String> = serde_wasm_bindgen::from_value(hashtags).map_err(|e| anyhow!("serde_wasm_bindgen::from_value error: {}", e))?;
            self.meta_post_manager().set_followed_hashtags(hashtags).await?;
        })
    }

    #[wasm_bindgen]
    pub async fn set_followed_hashtag_v1(&self, hashtag: String, is_followed: bool) -> Result<(), JsValue> {
        wasm_try!({
            self.meta_post_manager().set_followed_hashtag(hashtag, is_followed).await?;
        })
    }

    // ------------------------------------------------------------------
    // MetaPostV1 — unified config publish
    // ------------------------------------------------------------------

    #[wasm_bindgen]
    pub async fn submit_meta_post_v1(&self) -> Result<(), JsValue> {
        wasm_try!({
            self.hashiverse_client.submit_meta_post().await?;
        })
    }

    #[wasm_bindgen]
    pub async fn ensure_meta_post_in_current_bucket_v1(&self) -> Result<(), JsValue> {
        wasm_try!({
            self.hashiverse_client.ensure_meta_post_in_current_bucket().await?;
        })
    }

    // ------------------------------------------------------------------
    // Content thresholds
    // ------------------------------------------------------------------

    #[wasm_bindgen]
    pub async fn get_content_thresholds_v1(&self) -> Result<JsValue, JsValue> {
        wasm_try!({
            let thresholds = self.meta_post_manager().get_content_thresholds().await?;
            // serde_wasm_bindgen serializes HashMaps as JS Map (not plain object) and rejects
            // non-string keys with serialize_maps_as_objects.  Convert to String keys so the
            // result is a plain JS object matching TS Record<number, number>.
            let thresholds_js: std::collections::HashMap<String, u32> = thresholds.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
            let serializer = serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true);
            thresholds_js.serialize(&serializer).map_err(|e| anyhow!("serde_wasm_bindgen error: {}", e))?
        })
    }

    #[wasm_bindgen]
    pub async fn set_content_thresholds_v1(&self, thresholds: JsValue) -> Result<(), JsValue> {
        wasm_try!({
            let thresholds_str: std::collections::HashMap<String, u32> = serde_wasm_bindgen::from_value(thresholds).map_err(|e| anyhow!("serde_wasm_bindgen error: {}", e))?;
            let thresholds: std::collections::HashMap<u8, u32> = thresholds_str.into_iter()
                .map(|(k, v)| Ok((k.parse::<u8>().map_err(|e| anyhow!("invalid feedback_type key: {}", e))?, v)))
                .collect::<anyhow::Result<_>>()?;
            self.meta_post_manager().set_content_thresholds(thresholds).await?;
        })
    }

    // ------------------------------------------------------------------
    // Skip warnings for followed
    // ------------------------------------------------------------------

    #[wasm_bindgen]
    pub async fn get_skip_warnings_for_followed_v1(&self) -> Result<bool, JsValue> {
        wasm_try!({
            self.meta_post_manager().get_skip_warnings_for_followed().await?
        })
    }

    #[wasm_bindgen]
    pub async fn set_skip_warnings_for_followed_v1(&self, value: bool) -> Result<(), JsValue> {
        wasm_try!({
            self.meta_post_manager().set_skip_warnings_for_followed(value).await?;
        })
    }

    #[wasm_bindgen]
    pub async fn fetch_url_preview_v1(&self, url: String) -> Result<UrlPreview, JsValue> {
        wasm_try!({
            let preview = self.hashiverse_client.fetch_url_preview(&url).await?;
            UrlPreview {
                url: preview.url,
                title: preview.title,
                description: preview.description,
                image_url: preview.image_url,
            }
        })
    }

    #[wasm_bindgen]
    pub async fn fetch_trending_hashtags_v1(&self, limit: u16) -> Result<TrendingHashtagsFetchResponse, JsValue> {
        wasm_try!({
            let response = self.hashiverse_client.fetch_trending_hashtags(limit).await?;
            TrendingHashtagsFetchResponse {
                trending_hashtags: response.trending_hashtags.into_iter().map(|entry| TrendingHashtag {
                    hashtag: entry.hashtag,
                    count: entry.count,
                }).collect(),
            }
        })
    }
}

#[derive(Tsify, Serialize, Deserialize)]
#[tsify(into_wasm_abi)]
pub struct SingleTimelineGetMoreV1Response {
    pub posts: Vec<Post>,
    pub oldest_processed_time_millis: Option<i64>,
}

#[derive(Tsify, Serialize, Deserialize)]
#[tsify(into_wasm_abi)]
pub struct Post {
    pub post_id: String,
    pub time_millis: i64,
    pub client_id: String,
    pub bucket_location: String,
    pub post: String,
    pub encoded_post_header_hex: String, // contains the hex-encoded EncodedPost without the post body
    pub healed: bool, // true if the bundle header marks this post as healed (re-uploaded after loss); the displayed time may be inaccurate
}

#[derive(Tsify, Serialize, Deserialize)]
#[tsify(into_wasm_abi)]
pub struct Bio {
    pub client_id: String,
    pub nickname: String,
    pub status: String,
    pub selfie: String,
    pub avatar: String,
}

#[derive(Tsify, Serialize, Deserialize)]
#[tsify(into_wasm_abi)]
pub struct UrlPreview {
    pub url: String,
    pub title: String,
    pub description: String,
    pub image_url: String,
}

#[derive(Tsify, Serialize, Deserialize)]
#[tsify(into_wasm_abi)]
pub struct TrendingHashtag {
    pub hashtag: String,
    pub count: u64,
}

#[derive(Tsify, Serialize, Deserialize)]
#[tsify(into_wasm_abi)]
pub struct TrendingHashtagsFetchResponse {
    pub trending_hashtags: Vec<TrendingHashtag>,
}

#[derive(Tsify, Serialize, Deserialize)]
#[tsify(into_wasm_abi)]
pub struct PeerInfoV1 {
    pub peer_id_hex: String,
    pub address: String,
    pub version: String,
    pub timestamp_millis: i64,
    pub pow_initial: u8,
    pub pow_current_day: u8,
    pub pow_current_month: u8,
}

#[derive(Tsify, Serialize, Deserialize)]
#[tsify(into_wasm_abi)]
pub struct PowJobStatusV1 {
    pub label: String,
    pub pow_min: u8,
    pub best_pow_so_far: u8,
}

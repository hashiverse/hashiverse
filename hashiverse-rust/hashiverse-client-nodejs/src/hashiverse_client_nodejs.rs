use hashiverse_lib::client::args::Args;
use hashiverse_lib::client::client_storage::sqlite_client_storage::SqliteClientStorage;
use hashiverse_lib::client::hashiverse_client::HashiverseClient as LibHashiverseClient;
use hashiverse_lib::client::key_locker::disk_key_locker::{DiskKeyLocker, DiskKeyLockerManager};
use hashiverse_lib::client::key_locker::key_locker::KeyLockerManager;
use hashiverse_lib::protocol::posting::encoded_post::EncodedPostV1;
use hashiverse_lib::tools::buckets::{BucketLocation, BucketType};
use hashiverse_lib::tools::pow_generator::native_parallel_pow_generator::NativeParallelPowGenerator;
use hashiverse_lib::tools::runtime_services::RuntimeServices;
use hashiverse_lib::tools::time_provider::time_provider::RealTimeProvider;
use hashiverse_lib::tools::types::Id;
use hashiverse_lib::transport::bootstrap_provider::bootstrap_provider::BootstrapProvider;
use hashiverse_lib::transport::bootstrap_provider::manual_bootstrap_provider::ManualBootstrapProvider;
use hashiverse_lib::transport::partial_https_transport::PartialHttpsTransportFactory;
use bytes::Bytes;
use log::warn;
use napi_derive::napi;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::runtime::Runtime;

use crate::ensure_rustls_init;
use crate::napi_err::{anyhow_to_napi, join_err_to_napi};
use crate::napi_try;

// ---------------------------------------------------------------------------
// Data classes (POJOs — pass-by-value across the JS boundary)
// ---------------------------------------------------------------------------

#[napi(object)]
#[derive(Clone)]
pub struct Post {
    pub post_id: String,
    pub time_millis: i64,
    pub client_id: String,
    pub bucket_location: String,
    pub post: String,
    pub encoded_post_header_hex: String,
    pub healed: bool,
}

#[napi(object)]
#[derive(Clone)]
pub struct Bio {
    pub client_id: String,
    pub nickname: String,
    pub status: String,
    pub selfie: String,
    pub avatar: String,
}

#[napi(object)]
#[derive(Clone)]
pub struct UrlPreview {
    pub url: String,
    pub title: String,
    pub description: String,
    pub image_url: String,
}

#[napi(object)]
#[derive(Clone)]
pub struct TrendingHashtag {
    pub hashtag: String,
    pub count: u32,
}

#[napi(object)]
#[derive(Clone)]
pub struct TimelineResponse {
    pub posts: Vec<Post>,
    pub oldest_processed_time_millis: i64,
}

// ---------------------------------------------------------------------------
// Constructor options
// ---------------------------------------------------------------------------

#[napi(object)]
pub struct CreateFromKeyphraseOptions {
    pub key_phrase: String,
    pub data_dir: String,
    pub passphrase: Option<String>,
    pub bootstrap_addresses: Option<Vec<String>>,
}

#[napi(object)]
pub struct CreateFromStoredKeyOptions {
    pub client_id_hex: String,
    pub data_dir: String,
    pub passphrase: Option<String>,
    pub bootstrap_addresses: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Main client wrapper
// ---------------------------------------------------------------------------

#[napi]
pub struct HashiverseClient {
    runtime: Arc<Runtime>,
    inner: Arc<LibHashiverseClient>,
    key_locker_manager: Arc<DiskKeyLockerManager>,
    logged_in: bool,
}

impl HashiverseClient {
    fn create_from_xxx(
        runtime: Arc<Runtime>,
        data_dir: PathBuf,
        key_locker_manager: Arc<DiskKeyLockerManager>,
        key_locker: Arc<DiskKeyLocker>,
        bootstrap_addresses: Option<Vec<String>>,
        logged_in: bool,
    ) -> anyhow::Result<Self> {
        runtime.block_on(async {
            let client_storage_dir = data_dir.join("client_storage");
            std::fs::create_dir_all(&client_storage_dir)?;

            let time_provider = Arc::new(RealTimeProvider);
            let bootstrap_provider: Arc<dyn BootstrapProvider> = match bootstrap_addresses {
                Some(addresses) => ManualBootstrapProvider::new(addresses),
                None => {
                    use hashiverse_lib::transport::bootstrap_provider::dnssec_bootstrap_provider::DnssecBootstrapProvider;
                    Arc::new(DnssecBootstrapProvider::new())
                }
            };
            let transport_factory = Arc::new(PartialHttpsTransportFactory::new(bootstrap_provider));
            let pow_generator = Arc::new(NativeParallelPowGenerator::new());

            let runtime_services = Arc::new(RuntimeServices {
                time_provider,
                transport_factory,
                pow_generator,
            });

            let client_storage = SqliteClientStorage::new(client_storage_dir).await?;
            let hashiverse_client = LibHashiverseClient::new(runtime_services, client_storage, key_locker, Args::default()).await?;

            anyhow::Ok(HashiverseClient {
                runtime: runtime.clone(),
                inner: Arc::new(hashiverse_client),
                key_locker_manager,
                logged_in,
            })
        })
    }

    fn post_process_timeline_posts(
        encoded_posts: Vec<(BucketLocation, EncodedPostV1, Bytes, bool)>,
        oldest_processed_time_millis: hashiverse_lib::tools::time::TimeMillis,
    ) -> anyhow::Result<TimelineResponse> {
        let posts = encoded_posts
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
            .collect();

        Ok(TimelineResponse {
            posts,
            oldest_processed_time_millis: oldest_processed_time_millis.0,
        })
    }
}

#[napi]
impl HashiverseClient {
    // --- Construction ---

    #[napi(factory)]
    pub async fn create_from_keyphrase(opts: CreateFromKeyphraseOptions) -> napi::Result<Self> {
        ensure_rustls_init();

        let CreateFromKeyphraseOptions { key_phrase, data_dir, passphrase, bootstrap_addresses } = opts;
        let passphrase = passphrase.unwrap_or_default();

        tokio::task::spawn_blocking(move || -> anyhow::Result<Self> {
            let runtime = Arc::new(tokio::runtime::Builder::new_multi_thread().enable_all().build()?);

            let data_dir = PathBuf::from(shellexpand::tilde(&data_dir).into_owned());
            std::fs::create_dir_all(&data_dir)?;

            let key_locker_manager = DiskKeyLockerManager::with_data_dir(data_dir.clone(), passphrase)?;
            let logged_in = !key_phrase.is_empty();
            let key_locker: Arc<DiskKeyLocker> = runtime.block_on(key_locker_manager.create(key_phrase))?;

            HashiverseClient::create_from_xxx(runtime, data_dir, key_locker_manager, key_locker, bootstrap_addresses, logged_in)
        })
        .await
        .map_err(join_err_to_napi)?
        .map_err(anyhow_to_napi)
    }

    #[napi(factory)]
    pub async fn create_from_stored_key(opts: CreateFromStoredKeyOptions) -> napi::Result<Self> {
        ensure_rustls_init();

        let CreateFromStoredKeyOptions { client_id_hex, data_dir, passphrase, bootstrap_addresses } = opts;
        let passphrase = passphrase.unwrap_or_default();

        tokio::task::spawn_blocking(move || -> anyhow::Result<Self> {
            let runtime = Arc::new(tokio::runtime::Builder::new_multi_thread().enable_all().build()?);

            let data_dir = PathBuf::from(shellexpand::tilde(&data_dir).into_owned());
            std::fs::create_dir_all(&data_dir)?;

            let key_locker_manager = DiskKeyLockerManager::with_data_dir(data_dir.clone(), passphrase)?;
            let key_locker: Arc<DiskKeyLocker> = runtime.block_on(key_locker_manager.switch(client_id_hex))?;

            HashiverseClient::create_from_xxx(runtime, data_dir, key_locker_manager, key_locker, bootstrap_addresses, true)
        })
        .await
        .map_err(join_err_to_napi)?
        .map_err(anyhow_to_napi)
    }

    // --- Properties ---

    #[napi(getter)]
    pub fn client_id(&self) -> String {
        self.inner.client_id().id_hex()
    }

    #[napi(getter)]
    pub fn logged_in(&self) -> bool {
        self.logged_in
    }

    // --- Key management ---

    #[napi]
    pub async fn list_stored_keys(&self) -> napi::Result<Vec<String>> {
        let key_locker_manager = self.key_locker_manager.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            key_locker_manager.list().await?
        })
    }

    #[napi]
    pub async fn delete_stored_key(&self, key_public: String) -> napi::Result<()> {
        let key_locker_manager = self.key_locker_manager.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            key_locker_manager.delete(key_public).await?;
        })
    }

    #[napi]
    pub async fn delete_all_stored_keys(&self) -> napi::Result<()> {
        let key_locker_manager = self.key_locker_manager.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            key_locker_manager.reset().await?;
        })
    }

    // --- Posting ---
    //
    // submit_post takes raw hashiverse-flavoured HTML and submits it as-is.
    // No preprocessing magic in the wrapper — JS callers compose the body
    // themselves using the convertTextToHashiverseHtml* free functions for
    // hashtag, mention, URL preview, and full-text conversion. Single source
    // of truth for the canonical HTML schema lives in hashiverse-lib's
    // plain_text_post.rs.

    #[napi]
    pub async fn submit_post(&self, post_html: String) -> napi::Result<()> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            client.submit_post(&post_html).await?;
        })
    }

    #[napi]
    pub async fn set_bio(&self, nickname: String, status: String, selfie: String, avatar: String) -> napi::Result<()> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            client.meta_post_manager().set_bio(nickname, status, selfie, avatar).await?;
        })
    }

    #[napi]
    pub async fn submit_feedback(&self, bucket_location: String, post_id: String, feedback_type: u8) -> napi::Result<()> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            let bucket_location = BucketLocation::from_html_attr(&bucket_location)?;
            let post_id = Id::from_hex_str(&post_id)?;
            client.submit_feedback(bucket_location, post_id, feedback_type).await?;
        })
    }

    // --- Post retrieval ---

    #[napi]
    pub async fn get_post(&self, bucket_location: String, post_id: String) -> napi::Result<Post> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            let bucket_location_parsed = BucketLocation::from_html_attr(&bucket_location)?;
            let post_id_parsed = Id::from_hex_str(&post_id)?;
            let (bucket_location, post, raw_bytes, healed) = client.get_post(bucket_location_parsed, &post_id_parsed).await?;
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

    #[napi]
    pub async fn get_post_feedbacks(&self, bucket_location: String, post_id: String) -> napi::Result<Vec<u32>> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            let bucket_location = BucketLocation::from_html_attr(&bucket_location)?;
            let post_id = Id::from_hex_str(&post_id)?;
            let post_feedbacks = client.get_post_feedbacks(bucket_location, post_id).await?;
            let feedbacks: Vec<u32> = post_feedbacks.iter().map(|&f| f.min(u32::MAX as u64) as u32).collect();
            feedbacks
        })
    }

    // --- Single timelines ---

    #[napi]
    pub async fn single_timeline_reset(&self) -> napi::Result<()> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            client.single_timeline_reset().await?;
        })
    }

    #[napi]
    pub async fn get_my_timeline(&self) -> napi::Result<TimelineResponse> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            let id = client.client_id().id;
            let (posts, oldest) = client.single_timeline_get_more(BucketType::User, &id).await?;
            HashiverseClient::post_process_timeline_posts(posts, oldest)?
        })
    }

    #[napi]
    pub async fn get_my_mentions_timeline(&self) -> napi::Result<TimelineResponse> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            let id = client.client_id().id;
            let (posts, oldest) = client.single_timeline_get_more(BucketType::Mention, &id).await?;
            HashiverseClient::post_process_timeline_posts(posts, oldest)?
        })
    }

    #[napi]
    pub async fn get_user_timeline(&self, client_id_hex: String) -> napi::Result<TimelineResponse> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            let id = Id::from_hex_str(&client_id_hex)?;
            let (posts, oldest) = client.single_timeline_get_more(BucketType::User, &id).await?;
            HashiverseClient::post_process_timeline_posts(posts, oldest)?
        })
    }

    #[napi]
    pub async fn get_user_mentions_timeline(&self, client_id_hex: String) -> napi::Result<TimelineResponse> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            let id = Id::from_hex_str(&client_id_hex)?;
            let (posts, oldest) = client.single_timeline_get_more(BucketType::Mention, &id).await?;
            HashiverseClient::post_process_timeline_posts(posts, oldest)?
        })
    }

    #[napi]
    pub async fn get_hashtag_timeline(&self, hashtag: String) -> napi::Result<TimelineResponse> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            let id = Id::from_hashtag_str(&hashtag)?;
            let (posts, oldest) = client.single_timeline_get_more(BucketType::Hashtag, &id).await?;
            HashiverseClient::post_process_timeline_posts(posts, oldest)?
        })
    }

    #[napi]
    pub async fn get_replies_timeline(&self, post_id: String) -> napi::Result<TimelineResponse> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            let id = Id::from_hex_str(&post_id)?;
            let (posts, oldest) = client.single_timeline_get_more(BucketType::ReplyToPost, &id).await?;
            HashiverseClient::post_process_timeline_posts(posts, oldest)?
        })
    }

    #[napi]
    pub async fn get_sequel_timeline(&self, post_id: String) -> napi::Result<TimelineResponse> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            let id = Id::from_hex_str(&post_id)?;
            let (posts, oldest) = client.single_timeline_get_more(BucketType::Sequel, &id).await?;
            HashiverseClient::post_process_timeline_posts(posts, oldest)?
        })
    }

    // --- Multiple timelines ---

    #[napi]
    pub async fn multiple_timeline_reset(&self) -> napi::Result<()> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            client.multiple_timeline_reset().await?;
        })
    }

    #[napi]
    pub async fn get_followed_users_timeline(&self) -> napi::Result<TimelineResponse> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            let ids = client.meta_post_manager().get_followed_client_ids().await?;
            let (posts, oldest) = client.multiple_timeline_get_more(BucketType::User, &ids).await?;
            HashiverseClient::post_process_timeline_posts(posts, oldest)?
        })
    }

    #[napi]
    pub async fn get_followed_hashtags_timeline(&self) -> napi::Result<TimelineResponse> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            let hashtags = client.meta_post_manager().get_followed_hashtags().await?;
            let ids = hashtags.iter().map(|h| Id::from_hashtag_str(h)).collect::<anyhow::Result<Vec<_>>>()?;
            let (posts, oldest) = client.multiple_timeline_get_more(BucketType::Hashtag, &ids).await?;
            HashiverseClient::post_process_timeline_posts(posts, oldest)?
        })
    }

    // --- Follows ---

    #[napi]
    pub async fn get_followed_users(&self) -> napi::Result<Vec<String>> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            let ids = client.meta_post_manager().get_followed_client_ids().await?;
            let hex_ids: Vec<String> = ids.into_iter().map(|id| id.to_hex_str()).collect();
            hex_ids
        })
    }

    #[napi]
    pub async fn set_followed_users(&self, client_ids: Vec<String>) -> napi::Result<()> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            let ids = client_ids.iter().map(|s| Id::from_hex_str(s)).collect::<anyhow::Result<Vec<_>>>()?;
            client.meta_post_manager().set_followed_client_ids(ids).await?;
        })
    }

    #[napi]
    pub async fn follow_user(&self, client_id: String, is_followed: bool) -> napi::Result<()> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            let id = Id::from_hex_str(&client_id)?;
            client.meta_post_manager().set_followed_client_id(id, is_followed).await?;
        })
    }

    #[napi]
    pub async fn get_followed_hashtags(&self) -> napi::Result<Vec<String>> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            client.meta_post_manager().get_followed_hashtags().await?
        })
    }

    #[napi]
    pub async fn set_followed_hashtags(&self, hashtags: Vec<String>) -> napi::Result<()> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            client.meta_post_manager().set_followed_hashtags(hashtags).await?;
        })
    }

    #[napi]
    pub async fn follow_hashtag(&self, hashtag: String, is_followed: bool) -> napi::Result<()> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            client.meta_post_manager().set_followed_hashtag(hashtag, is_followed).await?;
        })
    }

    // --- Bios ---

    #[napi]
    pub async fn get_bio(&self, id: String) -> napi::Result<Bio> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            let public = client.meta_post_manager().get_meta_post_public(Id::from_hex_str(&id)?).await?;
            match public {
                Some(public) => Bio {
                    client_id: id,
                    nickname: public.nickname.value.unwrap_or_default(),
                    status: public.status.value.unwrap_or_default(),
                    selfie: public.selfie.value.unwrap_or_default(),
                    avatar: public.avatar.value.unwrap_or_default(),
                },
                None => Bio {
                    client_id: id,
                    nickname: String::new(),
                    status: String::new(),
                    selfie: String::new(),
                    avatar: String::new(),
                },
            }
        })
    }

    #[napi]
    pub async fn get_all_bios(&self) -> napi::Result<Vec<Bio>> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            let bios = client.meta_post_manager().get_all_meta_post_publics().await?;
            let result: Vec<Bio> = bios
                .into_iter()
                .map(|(client_id, public)| Bio {
                    client_id,
                    nickname: public.nickname.value.unwrap_or_default(),
                    status: public.status.value.unwrap_or_default(),
                    selfie: public.selfie.value.unwrap_or_default(),
                    avatar: public.avatar.value.unwrap_or_default(),
                })
                .collect();
            result
        })
    }

    // --- MetaPostV1 ---

    #[napi]
    pub async fn submit_meta_post(&self) -> napi::Result<()> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            client.submit_meta_post().await?;
        })
    }

    #[napi]
    pub async fn ensure_meta_post_in_current_bucket(&self) -> napi::Result<()> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            client.ensure_meta_post_in_current_bucket().await?;
        })
    }

    // --- URL preview ---

    #[napi]
    pub async fn fetch_url_preview(&self, url: String) -> napi::Result<UrlPreview> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            let preview = client.fetch_url_preview(&url).await?;
            UrlPreview {
                url: preview.url,
                title: preview.title,
                description: preview.description,
                image_url: preview.image_url,
            }
        })
    }

    // --- Trending ---

    #[napi]
    pub async fn fetch_trending_hashtags(&self, limit: u16) -> napi::Result<Vec<TrendingHashtag>> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            let response = client.fetch_trending_hashtags(limit).await?;
            let trending: Vec<TrendingHashtag> = response
                .trending_hashtags
                .into_iter()
                .map(|entry| TrendingHashtag {
                    hashtag: entry.hashtag,
                    count: entry.count.min(u32::MAX as u64) as u32,
                })
                .collect();
            trending
        })
    }

    // --- Storage ---

    #[napi]
    pub async fn reset_storage(&self) -> napi::Result<()> {
        let client = self.inner.clone();
        let runtime = self.runtime.clone();
        napi_try!(runtime, {
            client.client_storage_reset().await?;
        })
    }
}

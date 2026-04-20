use hashiverse_lib::client::args::Args;
use hashiverse_lib::client::hashiverse_client::HashiverseClient;
use hashiverse_lib::client::client_storage::sqlite_client_storage::SqliteClientStorage;
use hashiverse_lib::client::key_locker::key_locker::KeyLockerManager;
use hashiverse_lib::client::key_locker::disk_key_locker::{DiskKeyLocker, DiskKeyLockerManager};
use hashiverse_lib::tools::buckets::{BucketLocation, BucketType};
use hashiverse_lib::tools::parallel_pow_generator::NativeParallelPowGenerator;
use hashiverse_lib::tools::runtime_services::RuntimeServices;
use hashiverse_lib::tools::time_provider::time_provider::RealTimeProvider;
use hashiverse_lib::tools::types::Id;
use hashiverse_lib::transport::bootstrap_provider::bootstrap_provider::BootstrapProvider;
use hashiverse_lib::transport::bootstrap_provider::manual_bootstrap_provider::ManualBootstrapProvider;
use hashiverse_lib::transport::partial_https_transport::PartialHttpsTransportFactory;
use hashiverse_lib::protocol::posting::encoded_post::EncodedPostV1;
use log::warn;
use pyo3::prelude::*;
use std::path::PathBuf;
use std::sync::Arc;
use bytes::Bytes;
use tokio::runtime::Runtime;
use crate::py_try::anyhow_to_py;

// ---------------------------------------------------------------------------
// Data classes
// ---------------------------------------------------------------------------

#[pyclass(get_all)]
#[derive(Clone)]
pub struct Post {
    pub post_id: String,
    pub time_millis: i64,
    pub client_id: String,
    pub bucket_location: String,
    pub post: String,
    pub encoded_post_header_hex: String,
}

#[pymethods]
impl Post {
    fn __repr__(&self) -> String {
        format!("Post(post_id='{}', client_id='{}', post='{}')", self.post_id, self.client_id, &self.post[..self.post.len().min(50)])
    }
}

#[pyclass(get_all)]
#[derive(Clone)]
pub struct Bio {
    pub client_id: String,
    pub nickname: String,
    pub status: String,
    pub selfie: String,
    pub avatar: String,
}

#[pymethods]
impl Bio {
    fn __repr__(&self) -> String {
        format!("Bio(client_id='{}', nickname='{}')", self.client_id, self.nickname)
    }
}

#[pyclass(get_all)]
#[derive(Clone)]
pub struct UrlPreview {
    pub url: String,
    pub title: String,
    pub description: String,
    pub image_url: String,
}

#[pyclass(get_all)]
#[derive(Clone)]
pub struct TrendingHashtag {
    pub hashtag: String,
    pub count: u64,
}

#[pyclass(get_all)]
#[derive(Clone)]
pub struct TimelineResponse {
    pub posts: Vec<Post>,
    pub oldest_processed_time_millis: i64,
}

// ---------------------------------------------------------------------------
// Main client wrapper
// ---------------------------------------------------------------------------

#[pyclass]
pub struct HashiverseClientPython {
    runtime: Arc<Runtime>,
    hashiverse_client: Arc<HashiverseClient>,
    key_locker_manager: Arc<DiskKeyLockerManager>,
    logged_in: bool,
}

impl HashiverseClientPython {
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
            let hashiverse_client = HashiverseClient::new(
                runtime_services,
                client_storage,
                key_locker,
                Args::default(),
            ).await?;

            anyhow::Ok(HashiverseClientPython {
                runtime: runtime.clone(),
                hashiverse_client: Arc::new(hashiverse_client),
                key_locker_manager,
                logged_in,
            })
        })
    }

    fn post_process_timeline_posts(
        encoded_posts: Vec<(BucketLocation, EncodedPostV1, Bytes)>,
        oldest_processed_time_millis: hashiverse_lib::tools::time::TimeMillis,
    ) -> anyhow::Result<TimelineResponse> {
        let posts = encoded_posts
            .into_iter()
            .filter_map(|(bucket_location, post, raw_bytes)| {
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
                })
            })
            .collect();

        Ok(TimelineResponse {
            posts,
            oldest_processed_time_millis: oldest_processed_time_millis.0,
        })
    }
}

#[pymethods]
impl HashiverseClientPython {
    // --- Construction ---

    #[staticmethod]
    #[pyo3(name = "create_from_keyphrase", signature = (key_phrase, data_dir, passphrase="", bootstrap_addresses=None))]
    fn create_from_keyphrase(
        py: Python<'_>,
        key_phrase: String,
        data_dir: String,
        passphrase: &str,
        bootstrap_addresses: Option<Vec<String>>,
    ) -> PyResult<Self> {
        py.allow_threads(|| {
            let runtime = Arc::new(
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| anyhow::anyhow!("Failed to create tokio runtime: {}", e))?,
            );

            let data_dir = PathBuf::from(shellexpand::tilde(&data_dir).into_owned());
            std::fs::create_dir_all(&data_dir)?;

            let key_locker_manager = DiskKeyLockerManager::with_data_dir(data_dir.clone(), passphrase.to_string())?;
            let logged_in = !key_phrase.is_empty();
            let key_locker: Arc<DiskKeyLocker> = runtime.block_on(key_locker_manager.create(key_phrase))?;

            Self::create_from_xxx(runtime, data_dir, key_locker_manager, key_locker, bootstrap_addresses, logged_in)
        })
        .map_err(anyhow_to_py)
    }

    #[staticmethod]
    #[pyo3(name = "create_from_stored_key", signature = (key_public, data_dir, passphrase="", bootstrap_addresses=None))]
    fn create_from_stored_key(
        py: Python<'_>,
        key_public: String,
        data_dir: String,
        passphrase: &str,
        bootstrap_addresses: Option<Vec<String>>,
    ) -> PyResult<Self> {
        py.allow_threads(|| {
            let runtime = Arc::new(
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| anyhow::anyhow!("Failed to create tokio runtime: {}", e))?,
            );

            let data_dir = PathBuf::from(shellexpand::tilde(&data_dir).into_owned());
            std::fs::create_dir_all(&data_dir)?;

            let key_locker_manager = DiskKeyLockerManager::with_data_dir(data_dir.clone(), passphrase.to_string())?;
            let key_locker: Arc<DiskKeyLocker> = runtime.block_on(key_locker_manager.switch(key_public))?;

            Self::create_from_xxx(runtime, data_dir, key_locker_manager, key_locker, bootstrap_addresses, true)
        })
        .map_err(anyhow_to_py)
    }

    // --- Properties ---

    #[getter]
    fn client_id(&self) -> String {
        self.hashiverse_client.client_id().id_hex()
    }

    #[getter]
    fn logged_in(&self) -> bool {
        self.logged_in
    }

    // --- Key management ---

    fn list_stored_keys(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        let key_locker_manager = self.key_locker_manager.clone();
        py_try!(py, self.runtime, {
            key_locker_manager.list().await?
        })
    }

    fn delete_stored_key(&self, py: Python<'_>, key_public: String) -> PyResult<()> {
        let key_locker_manager = self.key_locker_manager.clone();
        py_try!(py, self.runtime, {
            key_locker_manager.delete(key_public).await?;
        })
    }

    fn delete_all_stored_keys(&self, py: Python<'_>) -> PyResult<()> {
        let key_locker_manager = self.key_locker_manager.clone();
        py_try!(py, self.runtime, {
            key_locker_manager.reset().await?;
        })
    }

    // --- Posting ---

    fn post_with_preprocessing(&self, py: Python<'_>, post_text: String) -> PyResult<()> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            let post_html = hashiverse_lib::tools::plain_text_post::convert_text_to_hashiverse_html(&post_text);
            client.submit_post(&post_html).await?;
        })
    }

    fn post_without_preprocessing(&self, py: Python<'_>, post_html: String) -> PyResult<()> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            client.submit_post(&post_html).await?;
        })
    }

    fn set_bio(&self, py: Python<'_>, nickname: String, status: String, selfie: String, avatar: String) -> PyResult<()> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            client.meta_post_manager().set_bio(nickname, status, selfie, avatar).await?;
        })
    }

    fn submit_feedback(&self, py: Python<'_>, bucket_location: String, post_id: String, feedback_type: u8) -> PyResult<()> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            let bucket_location = BucketLocation::from_html_attr(&bucket_location)?;
            let post_id = Id::from_hex_str(&post_id)?;
            client.submit_feedback(bucket_location, post_id, feedback_type).await?;
        })
    }

    // --- Post retrieval ---

    fn get_post(&self, py: Python<'_>, bucket_location: String, post_id: String) -> PyResult<Post> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            let bucket_location_parsed = BucketLocation::from_html_attr(&bucket_location)?;
            let post_id_parsed = Id::from_hex_str(&post_id)?;
            let (bucket_location, post, raw_bytes) = client.get_post(bucket_location_parsed, &post_id_parsed).await?;
            let client_id = post.header.client_id()?;
            let encoded_post_header_hex = hex::encode(EncodedPostV1::bytes_without_body(raw_bytes)?);
            Post {
                post_id: post.post_id.to_hex_str(),
                time_millis: post.header.time_millis.0,
                client_id: client_id.id_hex(),
                bucket_location: bucket_location.to_html_attr(),
                post: post.post,
                encoded_post_header_hex,
            }
        })
    }

    fn get_post_feedbacks(&self, py: Python<'_>, bucket_location: String, post_id: String) -> PyResult<Vec<u32>> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            let bucket_location = BucketLocation::from_html_attr(&bucket_location)?;
            let post_id = Id::from_hex_str(&post_id)?;
            let post_feedbacks = client.get_post_feedbacks(bucket_location, post_id).await?;
            let feedbacks: Vec<u32> = post_feedbacks.iter().map(|&f| f.min(u32::MAX as u64) as u32).collect();
            feedbacks
        })
    }

    // --- Timelines ---

    fn single_timeline_reset(&self, py: Python<'_>) -> PyResult<()> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            client.single_timeline_reset().await?;
        })
    }

    fn get_my_timeline(&self, py: Python<'_>) -> PyResult<TimelineResponse> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            let id = client.client_id().id;
            let (posts, oldest) = client.single_timeline_get_more(BucketType::User, &id).await?;
            HashiverseClientPython::post_process_timeline_posts(posts, oldest)?
        })
    }

    fn get_my_mentions_timeline(&self, py: Python<'_>) -> PyResult<TimelineResponse> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            let id = client.client_id().id;
            let (posts, oldest) = client.single_timeline_get_more(BucketType::Mention, &id).await?;
            HashiverseClientPython::post_process_timeline_posts(posts, oldest)?
        })
    }

    fn get_user_timeline(&self, py: Python<'_>, client_id_hex: String) -> PyResult<TimelineResponse> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            let id = Id::from_hex_str(&client_id_hex)?;
            let (posts, oldest) = client.single_timeline_get_more(BucketType::User, &id).await?;
            HashiverseClientPython::post_process_timeline_posts(posts, oldest)?
        })
    }

    fn get_user_mentions_timeline(&self, py: Python<'_>, client_id_hex: String) -> PyResult<TimelineResponse> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            let id = Id::from_hex_str(&client_id_hex)?;
            let (posts, oldest) = client.single_timeline_get_more(BucketType::Mention, &id).await?;
            HashiverseClientPython::post_process_timeline_posts(posts, oldest)?
        })
    }

    fn get_hashtag_timeline(&self, py: Python<'_>, hashtag: String) -> PyResult<TimelineResponse> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            let id = Id::from_hashtag_str(&hashtag)?;
            let (posts, oldest) = client.single_timeline_get_more(BucketType::Hashtag, &id).await?;
            HashiverseClientPython::post_process_timeline_posts(posts, oldest)?
        })
    }

    fn get_replies_timeline(&self, py: Python<'_>, post_id: String) -> PyResult<TimelineResponse> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            let id = Id::from_hex_str(&post_id)?;
            let (posts, oldest) = client.single_timeline_get_more(BucketType::ReplyToPost, &id).await?;
            HashiverseClientPython::post_process_timeline_posts(posts, oldest)?
        })
    }

    fn get_sequel_timeline(&self, py: Python<'_>, post_id: String) -> PyResult<TimelineResponse> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            let id = Id::from_hex_str(&post_id)?;
            let (posts, oldest) = client.single_timeline_get_more(BucketType::Sequel, &id).await?;
            HashiverseClientPython::post_process_timeline_posts(posts, oldest)?
        })
    }

    // --- Multiple timelines ---

    fn multiple_timeline_reset(&self, py: Python<'_>) -> PyResult<()> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            client.multiple_timeline_reset().await?;
        })
    }

    fn get_followed_users_timeline(&self, py: Python<'_>) -> PyResult<TimelineResponse> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            let ids = client.meta_post_manager().get_followed_client_ids().await?;
            let (posts, oldest) = client.multiple_timeline_get_more(BucketType::User, &ids).await?;
            HashiverseClientPython::post_process_timeline_posts(posts, oldest)?
        })
    }

    fn get_followed_hashtags_timeline(&self, py: Python<'_>) -> PyResult<TimelineResponse> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            let hashtags = client.meta_post_manager().get_followed_hashtags().await?;
            let ids = hashtags.iter().map(|h| Id::from_hashtag_str(h)).collect::<anyhow::Result<Vec<_>>>()?;
            let (posts, oldest) = client.multiple_timeline_get_more(BucketType::Hashtag, &ids).await?;
            HashiverseClientPython::post_process_timeline_posts(posts, oldest)?
        })
    }

    // --- Follows ---

    fn get_followed_users(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            let ids = client.meta_post_manager().get_followed_client_ids().await?;
            let hex_ids: Vec<String> = ids.into_iter().map(|id| id.to_hex_str()).collect();
            hex_ids
        })
    }

    fn set_followed_users(&self, py: Python<'_>, client_ids: Vec<String>) -> PyResult<()> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            let ids = client_ids.iter().map(|s| Id::from_hex_str(s)).collect::<anyhow::Result<Vec<_>>>()?;
            client.meta_post_manager().set_followed_client_ids(ids).await?;
        })
    }

    fn follow_user(&self, py: Python<'_>, client_id: String, is_followed: bool) -> PyResult<()> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            let id = Id::from_hex_str(&client_id)?;
            client.meta_post_manager().set_followed_client_id(id, is_followed).await?;
        })
    }

    fn get_followed_hashtags(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            client.meta_post_manager().get_followed_hashtags().await?
        })
    }

    fn set_followed_hashtags(&self, py: Python<'_>, hashtags: Vec<String>) -> PyResult<()> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            client.meta_post_manager().set_followed_hashtags(hashtags).await?;
        })
    }

    fn follow_hashtag(&self, py: Python<'_>, hashtag: String, is_followed: bool) -> PyResult<()> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            client.meta_post_manager().set_followed_hashtag(hashtag, is_followed).await?;
        })
    }

    // --- Bios ---

    fn get_bio(&self, py: Python<'_>, id: String) -> PyResult<Bio> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
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

    fn get_all_bios(&self, py: Python<'_>) -> PyResult<Vec<Bio>> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            let bios = client.meta_post_manager().get_all_meta_post_publics().await?;
            let result: Vec<Bio> = bios.into_iter()
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

    fn submit_meta_post(&self, py: Python<'_>) -> PyResult<()> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            client.submit_meta_post().await?;
        })
    }

    fn ensure_meta_post_in_current_bucket(&self, py: Python<'_>) -> PyResult<()> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            client.ensure_meta_post_in_current_bucket().await?;
        })
    }

    // --- URL preview ---

    fn fetch_url_preview(&self, py: Python<'_>, url: String) -> PyResult<UrlPreview> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
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

    fn fetch_trending_hashtags(&self, py: Python<'_>, limit: u16) -> PyResult<Vec<TrendingHashtag>> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            let response = client.fetch_trending_hashtags(limit).await?;
            let trending: Vec<TrendingHashtag> = response.trending_hashtags.into_iter()
                .map(|entry| TrendingHashtag {
                    hashtag: entry.hashtag,
                    count: entry.count,
                })
                .collect();
            trending
        })
    }

    // --- Storage ---

    fn reset_storage(&self, py: Python<'_>) -> PyResult<()> {
        let client = self.hashiverse_client.clone();
        py_try!(py, self.runtime, {
            client.client_storage_reset().await?;
        })
    }
}

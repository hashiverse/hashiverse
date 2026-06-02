#![feature(try_blocks)]

use hashiverse_lib::anyhow_assert;
use hashiverse_lib::client::client_storage::mem_client_storage::MemClientStorage;
use hashiverse_lib::client::hashiverse_client::HashiverseClient;
use hashiverse_lib::client::key_locker::key_locker::KeyLockerManager;
use hashiverse_lib::client::key_locker::mem_key_locker::MemKeyLockerManager;
use hashiverse_lib::protocol::posting::encoded_post_feedback::{EncodedPostFeedbackV1, EncodedPostFeedbackViewV1};
use hashiverse_lib::tools::config;
use hashiverse_lib::tools::pow_generator::native_parallel_pow_generator::NativeParallelPowGenerator;
use hashiverse_lib::tools::runtime_services::RuntimeServices;
use hashiverse_lib::tools::time::MILLIS_IN_MINUTE;
use hashiverse_lib::tools::time_provider::time_provider::{ScaledTimeProvider, TimeProvider};
use hashiverse_lib::tools::tools::{configure_logging_with_time_provider, get_temp_dir, leading_agreement_bits_xor};
use hashiverse_lib::tools::types::{Id, Pow, Salt};
use hashiverse_lib::transport::bootstrap_provider::manual_bootstrap_provider::ManualBootstrapProvider;
use hashiverse_lib::transport::ddos::noop_ddos::NoopDdosProtection;
use hashiverse_lib::transport::mem_transport::MemTransportFactory;
use hashiverse_server_lib::environment::environment::EnvironmentFactory;
use hashiverse_server_lib::environment::mem_environment_store::MemEnvironmentFactory;
use hashiverse_server_lib::server::args::Args;
use hashiverse_server_lib::server::hashiverse_server::HashiverseServer;
use log::{info, warn};
use parking_lot::Mutex;
use std::sync::Arc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

const CLOCK_SCALE_FACTOR: f64 = 15.0 * 60.0;
const FEEDBACK_TYPE_LIKE: u8 = 1;
const INJECTED_POW: Pow = Pow(200);

#[tokio::test(flavor = "multi_thread")]
async fn test_healing_post_bundle_feedbacks() -> anyhow::Result<()> {
    let (_, temp_dir_path) = get_temp_dir()?;
    let time_provider = Arc::new(ScaledTimeProvider::new(CLOCK_SCALE_FACTOR));
    configure_logging_with_time_provider("trace", time_provider.clone());

    let cancellation_token = CancellationToken::new();
    let environment_factory = Arc::new(MemEnvironmentFactory::new(&temp_dir_path));
    let transport_factory = Arc::new(MemTransportFactory::new(NoopDdosProtection::default(), ManualBootstrapProvider::new(vec!["443".to_string()])));

    let server_join_set: Arc<Mutex<JoinSet<()>>> = Arc::new(Mutex::new(JoinSet::new()));
    let hashiverse_servers: Arc<Mutex<Vec<Arc<_>>>> = Arc::new(Mutex::new(Vec::new()));

    let pow_generator = Arc::new(NativeParallelPowGenerator::new());

    let add_another_server = {
        let time_provider = time_provider.clone();
        let environment_factory = environment_factory.clone();
        let transport_factory = transport_factory.clone();
        let pow_generator = pow_generator.clone();
        let cancellation_token = cancellation_token.clone();
        let hashiverse_servers = Arc::clone(&hashiverse_servers);
        let server_join_set = Arc::clone(&server_join_set);
        move |port: u16| {
            let runtime_services = Arc::new(RuntimeServices {
                time_provider: time_provider.clone(),
                transport_factory: transport_factory.clone(),
                pow_generator: pow_generator.clone(),
            });
            let environment_factory = environment_factory.clone();
            let cancellation_token = cancellation_token.clone();
            let hashiverse_servers = Arc::clone(&hashiverse_servers);
            let server_join_set = Arc::clone(&server_join_set);
            async move {
                let args = Args::default().with_port(port).with_force_local_network(true);
                let hashiverse_server = HashiverseServer::new(runtime_services, environment_factory, args).await?;
                hashiverse_servers.lock().push(Arc::clone(&hashiverse_server));
                server_join_set.lock().spawn(async move {
                    hashiverse_server.run(cancellation_token).await;
                });
                Ok::<(), anyhow::Error>(())
            }
        }
    };

    let hashiverse_client = {
        let args = hashiverse_lib::client::args::Args::default();
        let key_locker_manager = MemKeyLockerManager::new().await?;
        let key_locker = key_locker_manager.create("client 0".to_string()).await?;
        let client_storage = MemClientStorage::new().await?;
        let runtime_services = Arc::new(RuntimeServices {
            time_provider: time_provider.clone(),
            transport_factory: transport_factory.clone(),
            pow_generator: Arc::new(NativeParallelPowGenerator::new()),
        });
        Arc::new(HashiverseClient::new(runtime_services, client_storage, key_locker, args).await?)
    };

    let mut fiddler_join_set: JoinSet<anyhow::Result<()>> = JoinSet::new();
    {
        let time_provider = time_provider.clone();
        let hashiverse_servers = Arc::clone(&hashiverse_servers);

        fiddler_join_set.spawn(async move {
            scopeguard::defer! {
                warn!("--- Killing all the servers ------------------------------------------------");
                cancellation_token.cancel();
            }

            let try_result: anyhow::Result<()> = try {
                warn!("--- Starting servers -----------------------------------------------");
                add_another_server(443).await?;
                for i in 1..30u16 {
                    add_another_server(20000 + i).await?;
                }

                // Let Kademlia converge across all servers before posting
                time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(30)).await;

                warn!("--- Posting from client ------------------------------------------------");
                let (post_result, _) = hashiverse_client.submit_post("Feedback healing test!").await
                    .expect("Posting failed");
                anyhow_assert!(!post_result.is_empty(), "Post returned no commit tokens");
                let bucket_location = post_result[0].bucket_location.clone();
                let post_id: Id = post_result[0].post_id;

                // Wait for post bundles to settle
                time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(5)).await;

                // Find the servers that actually hold the bundle, sorted nearest-first.
                // These are the only servers that will accept feedback, so injection must target them.
                let bundle_holders: Vec<Arc<_>> = {
                    let time_millis = time_provider.current_time_millis();
                    let servers = hashiverse_servers.lock();
                    let mut with_distances: Vec<(Arc<_>, _)> = servers
                        .iter()
                        .filter_map(|s: &Arc<_>| {
                            let has = s.environment
                                .get_post_bundle_bytes(time_millis, &bucket_location.location_id)
                                .ok()
                                .flatten()
                                .is_some();
                            if has {
                                let dist = leading_agreement_bits_xor(&s.server_id.id.0, &bucket_location.location_id.0);
                                Some((Arc::clone(s), dist))
                            } else {
                                None
                            }
                        })
                        .collect();
                    with_distances.sort_by_key(|(_, d)| -(*d));
                    with_distances.into_iter().map(|(s, _)| s).collect()
                };

                anyhow_assert!(
                    bundle_holders.len() >= config::REDUNDANT_SERVERS_PER_POST,
                    "Expected at least {} bundle holders, got {}",
                    config::REDUNDANT_SERVERS_PER_POST,
                    bundle_holders.len()
                );
                warn!("--- {} servers hold the post bundle -----------------------------------", bundle_holders.len());

                // Inject high-pow feedback to the FAR half of bundle holders (by nearest-first order).
                // The NEAR half has no feedback yet and must receive it via healing.
                let inject_from = bundle_holders.len() / 2;
                anyhow_assert!(inject_from > 0, "Not enough bundle holders to split for injection (got {})", bundle_holders.len());

                let injected_feedback = EncodedPostFeedbackV1 {
                    post_id,
                    feedback_type: FEEDBACK_TYPE_LIKE,
                    salt: Salt::random(),
                    pow: INJECTED_POW,
                };

                warn!("--- Injecting feedback to {} far bundle holders (skipping {} nearest) ---",
                    bundle_holders.len() - inject_from, inject_from);
                for server in &bundle_holders[inject_from..] {
                    let time_millis = time_provider.current_time_millis();
                    server.environment.put_post_feedback_if_more_powerful(time_millis, &bucket_location.location_id, &injected_feedback)?;
                    warn!("  Injected on server {}", server.server_id.id);
                }

                warn!("--- Pumping healing via client feedback fetches --------------------");
                for pump in 1..=5 {
                    warn!("  Pump {} ---", pump);
                    // get_post_feedbacks fetches all feedback bundles from the nearest servers and
                    // fires heal_post_bundle_feedbacks in the background for any that are behind.
                    let _ = hashiverse_client.get_post_feedbacks(bucket_location.clone(), post_id).await;
                    time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(2)).await;
                }

                warn!("--- Checking all bundle-holding servers have the healed feedback ---");
                for (i, server) in bundle_holders.iter().enumerate() {
                    let time_millis = time_provider.current_time_millis();
                    let feedback_bytes = server.environment
                        .get_post_bundle_encoded_post_feedbacks_bytes(time_millis, &bucket_location.location_id)?;
                    let has_feedback = EncodedPostFeedbackViewV1::iter(&feedback_bytes).any(|v| {
                        v.is_ok_and(|v| {
                            v.post_id_bytes() == post_id.as_ref()
                                && v.feedback_type() == FEEDBACK_TYPE_LIKE
                                && v.pow() >= INJECTED_POW
                        })
                    });
                    anyhow_assert!(
                        has_feedback,
                        "Server {} (rank {}) missing healed feedback for post {}",
                        server.server_id.id, i, post_id
                    );
                }
                warn!("--- All {} bundle holders have the healed feedback -----------------", bundle_holders.len());

                time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(5)).await;
            };

            try_result
        });
    }

    {
        let mut had_a_failure = false;
        while let Some(res) = fiddler_join_set.join_next().await {
            match res {
                Ok(Ok(())) => info!("fiddler completed"),
                Ok(Err(e)) => { had_a_failure = true; warn!("fiddler failed: {}", e); }
                Err(e) => { had_a_failure = true; warn!("fiddler panicked: {}", e); }
            }
        }
        assert!(!had_a_failure, "The fiddler failed");
    }

    {
        let mut had_a_failure = false;
        let mut server_join_set = Arc::try_unwrap(server_join_set)
            .expect("fiddler should have dropped its server_join_set Arc clone")
            .into_inner();
        while let Some(res) = server_join_set.join_next().await {
            match res {
                Ok(()) => info!("server completed"),
                Err(e) => { had_a_failure = true; warn!("server failed: {}", e); }
            }
        }
        assert!(!had_a_failure, "A server task failed");
    }

    Ok(())
}

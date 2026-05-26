//! # Stateful peer walk by XOR distance
//!
//! `PeerIterator` is the execution primitive for every DHT operation: given a target
//! [`crate::tools::types::Id`] and a
//! [`crate::client::peer_tracker::peer_tracker::PeerTracker`], hand back peers in order
//! of decreasing closeness while remembering which ones have already been tried.
//!
//! Two knobs tune the walk:
//!
//! - **High-watermark** on [`crate::tools::tools::LeadingAgreementBits`] — once `N`
//!   iterations have passed without finding a peer closer than the best so far, the
//!   iterator gives up. This is the standard Kademlia "no further progress possible"
//!   signal.
//! - **Cache radius** — supplied by
//!   [`crate::client::caching::cache_radius_tracker`]. Peers closer than the recorded
//!   cache radius are skipped, because whatever we'd fetch from them is already cached
//!   further out; fetching again just hammers the closest nodes.

use crate::client::peer_tracker::peer_tracker::PeerTracker;
use crate::protocol::peer::Peer;
use crate::tools::tools;
use crate::tools::tools::LeadingAgreementBits;
use crate::tools::types::Id;
use log::warn;
use std::collections::HashSet;

pub struct PeerIterator<'a> {
    tracker: &'a mut PeerTracker,
    bucket_location_id: Id,
    max_iterations_since_high_watermark: usize,
    peers_already_queried: HashSet<Id>,
    high_watermark: LeadingAgreementBits,
    iterations_since_high_watermark: usize,
    cache_radius: Option<LeadingAgreementBits>,
}

impl<'a> PeerIterator<'a> {
    pub fn new(tracker: &'a mut PeerTracker, bucket_location_id: Id, max_iterations_since_high_watermark: usize, cache_radius: Option<LeadingAgreementBits>) -> Self {
        Self {
            tracker,
            bucket_location_id,
            max_iterations_since_high_watermark,
            peers_already_queried: HashSet::new(),
            high_watermark: 0,
            iterations_since_high_watermark: 0,
            cache_radius,
        }
    }
    pub fn next_peer(&mut self) -> Option<(Peer, LeadingAgreementBits)> {
        loop {
            let nearest_peer = self
                .tracker
                .peers()
                .iter()
                .filter(|peer| !self.peers_already_queried.contains(&peer.id))
                .map(|peer| (peer, tools::leading_agreement_bits_xor(&self.bucket_location_id.0, &peer.id.0)))
                .filter(|(_, lab)| self.cache_radius.is_none_or(|r| *lab < r))
                .max_by_key(|peer| peer.1);

            match nearest_peer {
                Some(nearest_peer) => {
                    self.peers_already_queried.insert(nearest_peer.0.id);

                    if nearest_peer.1 > self.high_watermark {
                        self.high_watermark = nearest_peer.1;
                        self.iterations_since_high_watermark = 0;
                    }
                    else {
                        self.iterations_since_high_watermark += 1;
                        if self.iterations_since_high_watermark > self.max_iterations_since_high_watermark {
                            return None;
                        }
                    }

                    // Each successful return opens one more ring of closer peers on the next call.
                    if let Some(r) = &mut self.cache_radius {
                        *r = (*r + 1).min(256);
                    }

                    return Some((nearest_peer.0.clone(), nearest_peer.1));
                }
                None => {
                    // No unvisited peer passes the current radius filter.
                    // If there are no unvisited peers at all, we are done.
                    let any_unvisited = self.tracker.peers().iter().any(|p| !self.peers_already_queried.contains(&p.id));
                    if !any_unvisited {
                        return None;
                    }
                    // Allow the next ring of closer peers and retry. If the cap is already
                    // hit, incrementing is a no-op and we'd spin forever; drop the filter
                    // so the unvisited peers (whose lab must be == 256) become eligible.
                    let r = &mut self.cache_radius?;
                    let new_r = (*r + 1).min(256);
                    if new_r == *r {
                        warn!(
                            "PeerIterator: cache_radius cap hit at {} with unvisited peers remaining — dropping filter. bucket_location_id={} unvisited_count={}",
                            *r,
                            self.bucket_location_id,
                            self.tracker.peers().iter().filter(|p| !self.peers_already_queried.contains(&p.id)).count(),
                        );
                        self.cache_radius = None;
                    }
                    else {
                        *r = new_r;
                    }
                }
            }
        }
    }

    pub fn iterations_since_high_watermark(&self) -> usize {
        self.iterations_since_high_watermark
    }

    pub fn add_peers(&mut self, peers: Vec<Peer>) {
        for peer in peers {
            if let Err(e) = self.tracker.add_peer(peer) {
                warn!("not adding invalid peer: {}", e);
            }
        }
    }
    pub fn remove_peer(&mut self, peer: &Peer) {
        self.tracker.remove_peer(peer);
    }
}

pub struct ConvergeToLocationVisitResult {
    pub done: bool,                  // Stop iterating
    pub peer_unavailable: bool,      // Indicate that this peer has a problem of some sort and should be removed
    pub peers_discovered: Vec<Peer>, // Supply some newly discovered Peers to add to the iterations
}

#[async_trait::async_trait]
pub trait ConvergeToLocationVisitor: Send + Sync {
    async fn on_peer(&mut self, peer: &Peer) -> anyhow::Result<ConvergeToLocationVisitResult>;
}

#[cfg(test)]
mod tests {
    use crate::client::client_storage::mem_client_storage::MemClientStorage;
    use crate::client::peer_tracker::peer_tracker::PeerTracker;
    use crate::tools::buckets::{BUCKET_DURATIONS, BucketLocation, BucketType, generate_bucket_location};
    use crate::tools::config;
    use crate::tools::runtime_services::RuntimeServices;
    use crate::tools::server_id::ServerId;
    use crate::tools::time::{DurationMillis, TimeMillis};
    use crate::tools::types::Id;

    #[tokio::test]
    async fn converge_basics_test() -> anyhow::Result<()> {
        let runtime_services = RuntimeServices::default_for_testing();
        let client_storage = MemClientStorage::new().await?;
        let mut peer_tracker = PeerTracker::new(runtime_services.clone(), client_storage.clone()).await?;

        const NUM_PEERS: usize = 100;

        {
            for _ in 0..NUM_PEERS {
                let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), config::SERVER_KEY_POW_MIN, true, runtime_services.pow_generator.as_ref()).await?;
                let peer = server_id.to_peer(runtime_services.time_provider.as_ref())?;
                peer_tracker.add_peer(peer)?;
            }
            assert_eq!(NUM_PEERS, peer_tracker.len());
        }

        // Now iterate through them all
        {
            let bucket_location = generate_bucket_location(BucketType::User, Id::random(), BUCKET_DURATIONS[0], runtime_services.time_provider.current_time_millis())?;
            let mut count = 0;
            let mut peer_iter = peer_tracker.iterate_to_location(bucket_location.location_id, usize::MAX, None).await?;
            while let Some(_peer) = peer_iter.next_peer() {
                count += 1;
            }
            assert_eq!(NUM_PEERS, count);
        };

        // Now iterate through them, but bail after the first
        {
            let bucket_location = generate_bucket_location(BucketType::User, Id::random(), BUCKET_DURATIONS[0], runtime_services.time_provider.current_time_millis())?;
            let mut count = 0;
            let mut peer_iter = peer_tracker.iterate_to_location(bucket_location.location_id, usize::MAX, None).await?;
            if peer_iter.next_peer().is_some() {
                count += 1;
            }
            assert_eq!(1, count);
        };

        // Now iterate through them, but delete half of them
        {
            let bucket_location = generate_bucket_location(BucketType::User, Id::random(), BUCKET_DURATIONS[0], runtime_services.time_provider.current_time_millis())?;
            let mut count = 0;
            let mut peer_iter = peer_tracker.iterate_to_location(bucket_location.location_id, usize::MAX, None).await?;
            while let Some((peer, _)) = peer_iter.next_peer() {
                count += 1;
                if 0 == count % 2 {
                    peer_iter.remove_peer(&peer);
                }
            }
            assert_eq!(NUM_PEERS, count);
            assert_eq!(NUM_PEERS / 2, peer_tracker.len());
        }

        Ok(())
    }

    #[tokio::test]
    async fn converge_termination_test() -> anyhow::Result<()> {
        let runtime_services = RuntimeServices::default_for_testing();
        let client_storage = MemClientStorage::new().await?;
        let mut peer_tracker = PeerTracker::new(runtime_services.clone(), client_storage.clone()).await?;

        const NUM_PEERS: usize = 100;

        {
            for _ in 0..NUM_PEERS {
                let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), config::SERVER_KEY_POW_MIN, true, runtime_services.pow_generator.as_ref()).await?;
                let peer = server_id.to_peer(runtime_services.time_provider.as_ref())?;
                peer_tracker.add_peer(peer)?;
            }
            assert_eq!(NUM_PEERS, peer_tracker.len());
        }

        {
            let bucket_location = generate_bucket_location(BucketType::User, Id::random(), BUCKET_DURATIONS[0], runtime_services.time_provider.current_time_millis())?;
            let mut count = 0;
            let mut peer_iter = peer_tracker.iterate_to_location(bucket_location.location_id, 3, None).await?;
            while let Some(_peer) = peer_iter.next_peer() {
                count += 1;
            }
            assert_eq!(3 + 1, count);
        }

        Ok(())
    }

    #[tokio::test]
    async fn converge_insertions_test() -> anyhow::Result<()> {
        let runtime_services = RuntimeServices::default_for_testing();
        let client_storage = MemClientStorage::new().await?;
        let mut peer_tracker = PeerTracker::new(runtime_services.clone(), client_storage.clone()).await?;

        const NUM_PEERS: usize = 100;

        {
            for _ in 0..NUM_PEERS {
                let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), config::SERVER_KEY_POW_MIN, true, runtime_services.pow_generator.as_ref()).await?;
                let peer = server_id.to_peer(runtime_services.time_provider.as_ref())?;
                peer_tracker.add_peer(peer)?;
            }
            assert_eq!(NUM_PEERS, peer_tracker.len());
        }

        // Now iterate through them, but add a few more peers
        {
            let bucket_location = generate_bucket_location(BucketType::User, Id::random(), BUCKET_DURATIONS[0], runtime_services.time_provider.current_time_millis())?;
            let mut count = 0;
            let mut peer_iter = peer_tracker.iterate_to_location(bucket_location.location_id, usize::MAX, None).await?;
            while let Some(_peer) = peer_iter.next_peer() {
                count += 1;

                if 0 == count % 10 {
                    let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), config::SERVER_KEY_POW_MIN, true, runtime_services.pow_generator.as_ref()).await?;
                    let peer = server_id.to_peer(runtime_services.time_provider.as_ref())?;
                    peer_iter.add_peers(vec![peer]);
                }

                if 50 == count {
                    break;
                }
            }

            assert_eq!(50, count);
            assert_eq!(NUM_PEERS + 5, peer_tracker.len());
        }

        Ok(())
    }

    #[tokio::test]
    async fn converge_targeting_test() -> anyhow::Result<()> {
        let runtime_services = RuntimeServices::default_for_testing();
        let client_storage = MemClientStorage::new().await?;
        let mut peer_tracker = PeerTracker::new(runtime_services.clone(), client_storage.clone()).await?;

        const NUM_PEERS: usize = 100;

        {
            for _ in 0..NUM_PEERS {
                let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), config::SERVER_KEY_POW_MIN, true, runtime_services.pow_generator.as_ref()).await?;
                let peer = server_id.to_peer(runtime_services.time_provider.as_ref())?;
                peer_tracker.add_peer(peer)?;
            }
            assert_eq!(NUM_PEERS, peer_tracker.len());
        }

        // This is the peer we are actually targeting
        let target_server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), config::SERVER_KEY_POW_MIN, true, runtime_services.pow_generator.as_ref()).await?;
        let target_peer = target_server_id.to_peer(runtime_services.time_provider.as_ref())?;

        {
            const PEER_DISCOVERY_I: usize = 37usize;
            const PEER_DISCOVERY_I_PLUS_1: usize = PEER_DISCOVERY_I + 1;

            let bucket_location = {
                let mut location_id = target_peer.id;
                for i in 10..31 {
                    location_id.0[i] = 0u8;
                }
                BucketLocation {
                    bucket_type: BucketType::User,
                    base_id: location_id,
                    duration: DurationMillis::zero(),
                    bucket_time_millis: TimeMillis::zero(),
                    location_id,
                }
            };

            let mut count = 0;
            let mut peer_iter = peer_tracker.iterate_to_location(bucket_location.location_id, usize::MAX, None).await?;
            while let Some((peer, _)) = peer_iter.next_peer() {
                count += 1;
                match count {
                    PEER_DISCOVERY_I => {
                        peer_iter.add_peers(vec![target_peer.clone()]);
                    }
                    PEER_DISCOVERY_I_PLUS_1 => {
                        if peer.id != target_peer.id {
                            anyhow::bail!("peer is not the one we expected");
                        }
                        break;
                    }
                    _ => {}
                }
            }

            assert_eq!(PEER_DISCOVERY_I_PLUS_1, count);
            assert_eq!(NUM_PEERS + 1, peer_tracker.len());
        }

        Ok(())
    }

    /// Verify that `cache_radius` starts by skipping peers inside the radius, then opens up
    /// one ring per step so that closer peers are eventually visited too.
    #[tokio::test]
    async fn converge_cache_radius_test() -> anyhow::Result<()> {
        let runtime_services = RuntimeServices::default_for_testing();
        let client_storage = MemClientStorage::new().await?;
        let mut peer_tracker = PeerTracker::new(runtime_services.clone(), client_storage.clone()).await?;

        let location_id = Id::zero();

        const NUM_PEERS: usize = 100;
        let mut labs_added: Vec<crate::tools::tools::LeadingAgreementBits> = Vec::new();
        for _ in 0..NUM_PEERS {
            let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), config::SERVER_KEY_POW_MIN, true, runtime_services.pow_generator.as_ref()).await?;
            let peer = server_id.to_peer(runtime_services.time_provider.as_ref())?;
            let lab = crate::tools::tools::leading_agreement_bits_xor(&location_id.0, &peer.id.0);
            labs_added.push(lab);
            peer_tracker.add_peer(peer)?;
        }
        assert_eq!(NUM_PEERS, peer_tracker.len());

        let mut sorted_labs = labs_added.clone();
        sorted_labs.sort();
        let cache_radius = sorted_labs[NUM_PEERS / 2];

        let mut labs_visited: Vec<crate::tools::tools::LeadingAgreementBits> = Vec::new();
        let mut peer_iter = peer_tracker.iterate_to_location(location_id, usize::MAX, Some(cache_radius)).await?;
        while let Some((_, lab)) = peer_iter.next_peer() {
            labs_visited.push(lab);
        }

        assert_eq!(NUM_PEERS, labs_visited.len(), "all peers should be visited");

        let has_outside_peers = labs_added.iter().any(|&lab| lab < cache_radius);
        if has_outside_peers {
            assert!(labs_visited[0] < cache_radius, "first peer should be outside the initial cache zone, got lab={} cache_radius={}", labs_visited[0], cache_radius);
        }

        Ok(())
    }
}

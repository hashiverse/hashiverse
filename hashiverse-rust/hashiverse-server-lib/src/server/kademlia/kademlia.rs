//! # Server-side Kademlia routing table
//!
//! A generic [`Kademlia<ID, PEER>`] DHT that the server uses to answer
//! `BootstrapV1` / `AnnounceV1` queries and to pick candidate peers for
//! post-bundle / feedback lookups.
//!
//! - **Up to 256 buckets** — one per bit of the id's XOR distance from this
//!   server's own id.
//! - **Per-bucket cap** — `max_peers_per_bucket` from
//!   [`hashiverse_lib::tools::config::SERVER_KADEMLIA_MAX_PEERS_PER_BUCKET`]. When a
//!   bucket fills, the oldest / least-recently-seen entry is evicted so fresh peers
//!   always get a slot.
//! - **Bucket placement** via
//!   [`hashiverse_lib::tools::tools::LeadingAgreementBits`] — same metric the client
//!   uses for its `PeerIterator`, so server-side fan-out matches client-side
//!   expectations.
//!
//! Generic over both the id type and the peer struct so tests can instantiate a
//! simpler `Kademlia` than the production `Kademlia<Id, Peer>`.

use hashiverse_lib::tools::time::TimeMillis;
use hashiverse_lib::tools::tools;
use hashiverse_lib::tools::tools::LeadingAgreementBits;
use std::cmp::min;

pub trait Peer<ID: AsRef<[u8]> + PartialEq>: Clone {
    fn id(&self) -> &ID;
    fn id_hex(&self) -> String {
        hex::encode(self.id())
    }
    fn score(&self, time_millis: TimeMillis) -> f64;
}

pub struct Kademlia<ID: AsRef<[u8]> + PartialEq, PEER: Peer<ID>> {
    id: ID,
    max_peers_per_bucket: usize,
    peer_buckets: Vec<Vec<PEER>>,
    peers_last_changed: TimeMillis,
}

impl<ID: AsRef<[u8]> + PartialEq, PEER: Peer<ID> + std::fmt::Display> Kademlia<ID, PEER> {
    const MAX_BUCKETS: usize = size_of::<ID>() * 8;

    pub fn new(id: ID, max_peers_per_bucket: usize) -> Self {
        let mut peer_buckets = Vec::with_capacity(Self::MAX_BUCKETS);
        for _ in 0..Self::MAX_BUCKETS {
            peer_buckets.push(Vec::with_capacity(max_peers_per_bucket));
        }

        Self {
            id,
            max_peers_per_bucket,
            peer_buckets,
            peers_last_changed: TimeMillis::zero(),
        }
    }

    pub fn len(&self) -> usize {
        let mut len = 0;
        for peer_bucket in &self.peer_buckets {
            len += peer_bucket.len();
        }

        len
    }

    pub fn is_empty(&self) -> bool {
        for peer_bucket in &self.peer_buckets {
            if !peer_bucket.is_empty() {
                return false;
            }
        }

        true
    }

    pub fn add_peer(&mut self, peer: PEER, time_millis: TimeMillis) -> anyhow::Result<()> {
        self.peers_last_changed = time_millis;
        let max_peers_per_bucket = self.max_peers_per_bucket;
        let peer_id = peer.id();
        let (peer_bucket, _peer_bucket_id) = self.get_bucket_for_id_mut(peer_id);
        // trace!("Adding into bucket={} peer={}", peer_bucket_id, peer);

        // Overwrite this peer if we already have it, but only if the new one is 'better'
        if let Some(i) = peer_bucket.iter().position(|p| *p.id() == *peer_id) {
            if peer_bucket[i].score(time_millis) < peer.score(time_millis) {
                // trace!("overwriting peer {}", peer.id_hex());
                peer_bucket[i] = peer;
            }
            return Ok(());
        }

        // Do we have room for more?
        if peer_bucket.len() < max_peers_per_bucket {
            peer_bucket.push(peer);
            return Ok(());
        }

        // Is the bucket full?  If so, possibly replace the peer with the lowest score
        else {
            let min_score_index = peer_bucket
                .iter()
                .enumerate()
                .min_by(|a, b| a.1.score(time_millis).total_cmp(&b.1.score(time_millis)))
                .map(|(i, _p)| i);

            if let Some(min_score_index) = min_score_index {
                if peer_bucket[min_score_index].score(time_millis) < peer.score(time_millis) {
                    peer_bucket[min_score_index] = peer;
                }
            }
        }

        Ok(())
    }

    pub fn remove_peer(&mut self, id: &ID, time_millis: TimeMillis) -> Option<PEER> {
        self.peers_last_changed = time_millis;
        let (peer_bucket, _) = self.get_bucket_for_id_mut(id);
        if let Some(i) = peer_bucket.iter().position(|p| *p.id() == *id) {
            return Some(peer_bucket.swap_remove(i));
        }
        None
    }

    pub fn get_peer_with_lowest_score(&self) -> Option<&PEER> {
        let mut lowest_score_peer: Option<&PEER> = None;
        let mut lowest_score: f64 = f64::MAX;

        for peer_bucket in &self.peer_buckets {
            for peer in peer_bucket {
                let score = peer.score(self.peers_last_changed);
                if score < lowest_score {
                    lowest_score = score;
                    lowest_score_peer = Some(peer);
                }
            }
        }

        lowest_score_peer
    }

    pub fn format_bucket_sizes(&self) -> String {
        format!(
            "[ {} ]",
            self.peer_buckets
                .iter()
                .map(|bucket| format!("{}", bucket.len()))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    fn get_bucket_for_id(&self, id: &ID) -> &Vec<PEER> {
        let peer_bucket_id = min(
            tools::leading_agreement_bits_xor(self.id.as_ref(), id.as_ref()) as usize,
            Self::MAX_BUCKETS - 1,
        );

        &self.peer_buckets[peer_bucket_id]
    }

    fn get_bucket_for_id_mut(&mut self, id: &ID) -> (&mut Vec<PEER>, usize) {
        let peer_bucket_id = min(
            tools::leading_agreement_bits_xor(self.id.as_ref(), id.as_ref()) as usize,
            Self::MAX_BUCKETS - 1,
        );

        (&mut self.peer_buckets[peer_bucket_id], peer_bucket_id)
    }

    pub fn get_peers_random(&self, max_peers: usize) -> Vec<PEER> {
        let mut peers = Vec::with_capacity(max_peers);

        // If we have too few peers, return them all
        if self.len() <= max_peers {
            for peer_bucket in &self.peer_buckets {
                for peer in peer_bucket {
                    peers.push(peer.clone());
                }
            }
            return peers;
        }

        // Otherwise return a smattering of peers
        loop {
            let peer_bucket = tools::random_element(&self.peer_buckets);
            if !peer_bucket.is_empty() {
                let peer = tools::random_element(peer_bucket);
                peers.push(peer.clone());

                if peers.len() >= max_peers {
                    return peers;
                }
            }
        }
    }

    pub fn get_all_peers(&self) -> Vec<PEER> {
        let mut peers = Vec::with_capacity(self.len());
        for peer_bucket in &self.peer_buckets {
            peers.extend(peer_bucket.iter().cloned());
        }
        peers
    }

    pub fn get_peer_buckets(&self) -> &Vec<Vec<PEER>> {
        &self.peer_buckets
    }

    pub fn get_peers_for_key(&self, id: &ID, max_peers: usize) -> (Vec<PEER>, bool) {
        let peer_bucket = self.get_bucket_for_id(id);

        // Compute (agreement, peer) pairs from the relevant source.
        // Fast path: all peers in this bucket are provably closer to the target than peers in any other bucket.
        // Slow path: not enough peers in the target bucket, scan all buckets.
        let mut agreement_peers: Vec<(LeadingAgreementBits, &PEER)> = if peer_bucket.len() >= max_peers {
            peer_bucket.iter()
                .map(|peer| (tools::leading_agreement_bits_xor(id.as_ref(), peer.id().as_ref()), peer))
                .collect()
        } else {
            self.peer_buckets.iter()
                .flat_map(|bucket| bucket.iter())
                .map(|peer| (tools::leading_agreement_bits_xor(id.as_ref(), peer.id().as_ref()), peer))
                .collect()
        };

        // Sort descending by agreement (closest first)
        agreement_peers.sort_unstable_by(|a, b| b.0.cmp(&a.0));

        // Find the threshold: include all peers tied with the max_peers-th entry
        let min_agreement = if agreement_peers.len() <= max_peers {
            LeadingAgreementBits::MIN
        } else {
            agreement_peers[max_peers - 1].0
        };

        let peers: Vec<PEER> = agreement_peers.iter()
            .take_while(|(agreement, _)| *agreement >= min_agreement)
            .map(|(_, peer)| (*peer).clone())
            .collect();
        let contains_self = peers.iter().any(|peer| *peer.id() == self.id);
        (peers, contains_self)
    }

    pub fn peers_last_changed(&self) -> TimeMillis {
        self.peers_last_changed
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use anyhow::{anyhow, Result};
    use log::trace;

    type ID = [u8; 2];

    #[derive(Clone, Debug, PartialEq)]
    struct Item {
        id: ID,
    }

    impl Item {
        fn new(id: ID) -> Self {
            Self { id }
        }

        fn new_random() -> Self {
            let mut id: ID = [0; size_of::<ID>()];
            tools::random_fill_bytes(&mut id);
            Self::new(id)
        }
    }

    impl std::fmt::Display for Item {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", hex::encode(&self.id))
        }
    }

    impl AsRef<[u8]> for Item {
        fn as_ref(&self) -> &[u8] {
            &self.id
        }
    }

    impl Peer<ID> for Item {
        fn id(&self) -> &ID {
            &self.id
        }

        fn score(&self, _time_millis: TimeMillis) -> f64 { self.id[0] as f64}
    }

    fn get_closest_peer(id: &Item, peers: &Vec<Item>) -> Result<Item> {
        let result = peers.iter().max_by(|a, b| {
            tools::leading_agreement_bits_xor(a.as_ref(), id.as_ref()).cmp(
                &tools::leading_agreement_bits_xor(b.as_ref(), id.as_ref()),
            )
        });

        result
            .cloned()
            .ok_or_else(|| anyhow!("should always have a closest peer!"))
    }

    #[tokio::test]
    pub async fn test_get_closest_peers_from_single_bucket() -> Result<()> {
        for _ in 0..1000 {
            let item = Item::new_random();
            trace!("server_id={}", item);

            let mut kademlia = Kademlia::<ID, Item>::new(item.id, 128);

            let num_peers = 64;
            let mut peers = Vec::with_capacity(num_peers);
            for _ in 0..num_peers {
                let peer = Item::new_random();
                peers.push(peer.clone());
                kademlia.add_peer(peer.clone(), TimeMillis::zero())?;
            }
            trace!("buckets={}", kademlia.format_bucket_sizes());

            for _ in 0..100 {
                let target_key = Item::new_random();
                let closest_peer = get_closest_peer(&target_key, &peers)?;
                let (closest_peers, _) = kademlia.get_peers_for_key(&target_key.id, 1);
                trace!(
                    "For target_key={} closest_peer={} closest_peers={}",
                    target_key,
                    closest_peer,
                    tools::format_vec(&closest_peers)
                );

                let dist1 = tools::leading_agreement_bits_xor(target_key.id(), closest_peer.id());
                let dist2 = tools::leading_agreement_bits_xor(target_key.id(), closest_peers[0].id());
                assert_eq!(dist1, dist2);
            }
        }
        Ok(())
    }
}

use crate::server::kademlia::kademlia::Kademlia;
use hashiverse_lib::protocol::peer::Peer;
use hashiverse_lib::tools::types::Id;

/// Build the `kademlia` subtree. v1 reports the total peer count across all
/// buckets — cheap (one walk of the bucket vector) and useful enough to spot
/// a node that has fallen off the DHT.
pub fn kademlia_stats_subtree(kademlia: &Kademlia<Id, Peer>) -> serde_json::Value {
    serde_json::json!({
        "peer_count": kademlia.len(),
    })
}

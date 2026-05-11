use hashiverse_lib::protocol::payload::payload::{PayloadRequestKind, PAYLOAD_REQUEST_KIND_COUNT};
use std::sync::atomic::{AtomicU64, Ordering};

/// Build the `requests` subtree: one numeric field per [`PayloadRequestKind`]
/// variant, keyed by its `Display` name, holding the running inbound counter.
///
/// Reads use `Ordering::Relaxed` — counters are advisory metrics, not
/// synchronisation primitives, and the snapshot doesn't need to be coherent
/// across kinds.
pub fn request_counts_subtree(counters: &[AtomicU64; PAYLOAD_REQUEST_KIND_COUNT]) -> serde_json::Value {
    let mut map = serde_json::Map::with_capacity(PAYLOAD_REQUEST_KIND_COUNT);
    for (index, counter) in counters.iter().enumerate() {
        let kind = match PayloadRequestKind::from_u16(index as u16) {
            Ok(kind) => kind,
            Err(_) => continue,
        };
        let count = counter.load(Ordering::Relaxed);
        map.insert(kind.to_string(), serde_json::Value::from(count));
    }
    serde_json::Value::Object(map)
}

//! # Protocol-wide constants
//!
//! One centralised place for every tunable number that affects on-network behaviour:
//! minimum proof-of-work for each RPC class, blob size limits, bootstrap domains,
//! DDoS thresholds, cache sizes, TLS / HTTP timeouts, bucket durations, certificate
//! renewal cadences, and so on.
//!
//! ## Testing vs production
//!
//! Two multipliers compensate automatically when running under `cfg(test)` or
//! `debug_assertions`:
//! - `POW_REDUCTION_FACTOR` slashes the mandatory PoW so tests finish quickly.
//! - `CLOCK_LENIENCE_FACTOR` widens acceptable clock drift so virtual-clock tests
//!   don't spuriously reject signed messages.
//!
//! Any constant here that participates in protocol validation must be **the same across
//! all peers** — changing a `POW_MINIMUM_*` value is effectively a soft fork.

pub const TESTING: bool = cfg!(any(test, debug_assertions));
pub const POW_REDUCTION_FACTOR: u8 = if TESTING { 16 } else { 1 };
pub const CLOCK_LENIENCE_FACTOR: i64 = if TESTING { 8 } else { 1 };

use crate::tools::time::{DurationMillis, MILLIS_IN_DAY, MILLIS_IN_HOUR, MILLIS_IN_MINUTE};
use crate::tools::types::Pow;

pub const PROTOCOL_MAX_BLOB_SIZE_REQUEST: usize = 4 * 1024 * 1024;
pub const PROTOCOL_MAX_BLOB_SIZE_RESPONSE: usize = 16 * 1024 * 1024;

pub const MINIMUM_PEERS_TO_STOP_BOOTSTRAPPING: usize = 8;
pub const MILLIS_TO_WAIT_BETWEEN_BOOTSTRAPS: DurationMillis = MILLIS_IN_MINUTE.const_mul(2);
pub const MILLIS_TO_WAIT_BETWEEN_ANNOUNCES: DurationMillis = MILLIS_IN_MINUTE.const_mul(1);
pub const MILLIS_TO_WAIT_BETWEEN_PEER_DUMPS: DurationMillis = MILLIS_IN_MINUTE.const_mul(15);

pub const SERVER_DDOS_IPSET_SET_NAME: &str = "hashiverse_ddos_blacklist";
pub const SERVER_DDOS_SCORE_THRESHOLD: f64 = 15.0;     // Score at which IP is banned
pub const SERVER_DDOS_DECAY_PER_SECOND: f64 = 0.5;     // Score points drained per second (steady state for 1 req/sec = ~10, for 1 req/min = ~0.17)
pub const SERVER_DDOS_BAD_REQUEST_PENALTY: f64 = 5.0;   // Points added per bad request
// Maximum simultaneous connections from a single IP.  Prevents one IP from monopolising all connection slots.
pub const SERVER_DDOS_MAX_CONNECTIONS_PER_IP: usize = 4;

pub const POW_MINIMUM_PER_RPC_SERVER_KNOWN: Pow = Pow(16 / POW_REDUCTION_FACTOR);
pub const POW_MINIMUM_PER_RPC_SERVER_UNKNOWN: Pow = Pow(POW_MINIMUM_PER_RPC_SERVER_KNOWN.0 + 2);
pub const POW_MINIMUM_PER_POST: Pow = Pow(POW_MINIMUM_PER_RPC_SERVER_KNOWN.0 + 2);
pub const POW_MINIMUM_PER_FEEDBACK: Pow = Pow(POW_MINIMUM_PER_RPC_SERVER_KNOWN.0 + 4);
pub const POW_MINIMUM_PER_URL_FETCH: Pow = Pow(POW_MINIMUM_PER_RPC_SERVER_KNOWN.0 + 3);
pub const POW_MINIMUM_PER_PEER_STATS: Pow = Pow(POW_MINIMUM_PER_RPC_SERVER_KNOWN.0 + 6);

pub const POW_MAX_CLOCK_DRIFT_MILLIS: DurationMillis = MILLIS_IN_MINUTE.const_mul(5).const_mul(CLOCK_LENIENCE_FACTOR);

pub const USE_PRODUCTION_LETS_ENCRYPT: bool = true;
pub const MILLIS_TO_WAIT_BETWEEN_CERT_RENEWAL_CHECKS: DurationMillis = MILLIS_IN_HOUR.const_mul(1);
pub const MILLIS_TO_WAIT_BETWEEN_CERT_RENEWALS: DurationMillis = MILLIS_IN_DAY.const_mul(5);
pub const MILLIS_TO_WAIT_BETWEEN_CERT_RENEWAL_FAILURES: DurationMillis = MILLIS_IN_HOUR.const_mul(3);

pub const ENCODED_POST_BUNDLE_V1_OVERFLOWED_NUM_POSTS: u8 = 20;
pub const ENCODED_POST_BUNDLE_V1_OVERFLOWED_NUM_POSTS_GRANTED: u8 = 3 * ENCODED_POST_BUNDLE_V1_OVERFLOWED_NUM_POSTS / 2;
pub const ENCODED_POST_BUNDLE_V1_ELAPSED_THRESHOLD_MILLIS: DurationMillis = MILLIS_IN_MINUTE.const_mul(1);

pub const ANNOUNCE_V1_NUM_PEERS: usize = 16;
pub const BOOTSTRAP_V1_NUM_PEERS: usize = 32;

// Across how many server should each post be stored?
pub const REDUNDANT_SERVERS_PER_POST: usize = 3;
pub const CLIENT_POST_TIMESTAMP_DELTA_THRESHOLD: DurationMillis =  MILLIS_IN_MINUTE.const_mul(10).const_mul(CLOCK_LENIENCE_FACTOR);
pub const CLIENT_POST_BUNDLE_CACHE_DURATION: DurationMillis = MILLIS_IN_MINUTE.const_mul(5);
pub const CLIENT_POST_BUNDLE_FEEDBACK_CACHE_DURATION: DurationMillis = MILLIS_IN_MINUTE.const_mul(15);

pub const SERVER_KEY_POW_MIN: Pow = Pow(32 / POW_REDUCTION_FACTOR);

pub const CLIENT_FEEDBACK_POW_NUMERAIRE: usize = 16 * 1024;

pub const TRANSPORT_BYTES_GATHERER_COMPACT_THRESHOLD: usize = 1024;

// Per-server hard cap on concurrent TCP connections.  Well below the default ulimit of 1,024 so a connection-exhaustion attack cannot starve the OS.
pub const HTTPS_SERVER_TRANSPORT_MAX_CONNECTIONS: usize = 512;

// Drop a TLS connection if the client hasn't completed the handshake within this many seconds.
pub const HTTPS_SERVER_TRANSPORT_TLS_HANDSHAKE_TIMEOUT_SECS: u64 = 8;

// Drop an HTTP/1.1 connection if the client hasn't finished sending request headers within this many seconds.  This is the primary defence against Slow Loris.
pub const HTTPS_SERVER_TRANSPORT_HEADER_READ_TIMEOUT_SECS: u64 = 8;

// Drop a connection if the client hasn't finished sending the request body within this many seconds.  This defends against body-level slow Loris (headers arrive fast, body trickles forever).
pub const HTTPS_SERVER_TRANSPORT_BODY_READ_TIMEOUT_SECS: u64 = 30;

// Maximum time to wait for in-flight connections to drain during graceful shutdown before forcibly aborting them.
pub const HTTPS_SERVER_TRANSPORT_SHUTDOWN_TIMEOUT_SECS: u64 = 5;

pub const BOOTSTRAP_DOMAINS: &[&str] = &["bootstrap.hashiverse.com", "bootstrap.hashiverse.eu", "bootstrap.hashiverse.ch"];

pub const SERVER_KADEMLIA_MAX_PEERS_PER_BUCKET: usize = 64;
pub const SERVER_POST_BUNDLE_CACHE_MAX_BYTES: u64 = 128 * 1024 * 1024;
pub const SERVER_POST_BUNDLE_CACHE_MAX_ORIGINATORS_PER_LOCATION: usize = 5;
pub const SERVER_POST_BUNDLE_FEEDBACK_CACHE_MAX_BYTES: u64 = 32 * 1024 * 1024;

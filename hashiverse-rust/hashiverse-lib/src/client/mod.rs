//! # Client-side protocol participation
//!
//! Everything a user-facing client needs to read, write, and synchronise with a
//! hashiverse network. [`hashiverse_client::HashiverseClient`] is the façade tying
//! the rest of the submodules together — peer tracking, post-bundle fetching and
//! healing, timelines, per-account signing identity, profile management, and the
//! pluggable persistence layer.

pub mod args;
pub mod caching;
pub mod hashiverse_client;
pub mod peer_tracker;
pub mod client_storage;
pub mod key_locker;
pub mod post_bundle;
pub mod timeline;
pub mod meta_post;

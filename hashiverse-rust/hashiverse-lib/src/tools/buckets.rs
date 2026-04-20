//! # Hierarchical time buckets for post storage and discovery
//!
//! Posts on hashiverse aren't stored by post id — they are stored inside *buckets* keyed
//! by `(bucket_type, base_id, duration, bucket_time_millis)`, and every bucket deterministically
//! hashes down to a single [`Id`] known as its `location_id`. That location_id is what the
//! Kademlia DHT uses to decide which servers are responsible for holding the bucket.
//!
//! ## Bucket types
//!
//! [`BucketType`] picks what the bucket is *about*:
//! - `User` — posts authored by a specific client id
//! - `Hashtag` — posts tagged with a given hashtag id
//! - `Mention` — posts that mention a given client id
//! - `ReplyToPost` — replies to a specific parent post
//! - `Sequel` — long-running threads that continue from an earlier post
//!
//! ## Duration hierarchy
//!
//! [`BUCKET_DURATIONS`] is a coarse-to-fine ladder (year → month → week → day → 6h → 1h →
//! 15m → 5m → 1m) chosen so that each level fans out ~4–6x into the next. When a bucket
//! overflows at one granularity the client recurses into the finer granularity; the fan-out
//! factor keeps the worst case from exploding into thousands of tiny buckets.
//!
//! Sequel buckets start at year granularity (cheaper discoverability for long-lived threads);
//! all other types start at month so that a single location_id isn't responsible for a full
//! year of heavy activity — see [`bucket_durations_for_type`].
//!
//! ## Types
//!
//! - [`Bucket`] — a duration-only description of a bucket.
//! - [`BucketLocation`] — the full `(bucket_type, base_id, duration, bucket_time_millis,
//!   location_id)` tuple with deterministic hashing, self-verification via
//!   [`BucketLocation::validate`], and a versioned tilde-delimited serialisation for use in
//!   HTML attributes and URLs.

use crate::tools::hashing;
use crate::tools::time::{DurationMillis, MILLIS_IN_DAY, MILLIS_IN_HOUR, MILLIS_IN_MINUTE, MILLIS_IN_MONTH, MILLIS_IN_WEEK, MILLIS_IN_YEAR, TimeMillis};
use crate::tools::types::Id;
use derive_more::Display;
use serde::{Deserialize, Serialize};

pub struct Bucket {
    pub duration: DurationMillis,
}

#[repr(u8)]
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy, Display)]
pub enum BucketType {
    User = 0,
    Hashtag = 1,
    Mention = 2,
    ReplyToPost = 3,
    Sequel = 4,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Display)]
#[display(fmt = "[ location_id: {}, bucket_type: {}, base_id: {}, duration: {}, bucket_time_millis: {}]", location_id, bucket_type, base_id, duration, bucket_time_millis)]
pub struct BucketLocation {
    pub bucket_type: BucketType,
    pub base_id: Id,
    pub duration: DurationMillis, // The bucket granularity
    pub bucket_time_millis: TimeMillis,    // The timestamp at the beginning of the bucket (i.e. rounded down by duration)
    pub location_id: Id,          // The resulting location_id
}

impl BucketLocation {
    pub fn round_down_to_bucket_start(timestamp: TimeMillis, duration: DurationMillis) -> TimeMillis {
        TimeMillis((timestamp.0 / duration.0) * duration.0)
    }

    pub fn new(bucket_type: BucketType, base_id: Id, duration: DurationMillis, timestamp: TimeMillis) -> anyhow::Result<Self> {
        let bucket_time_millis = Self::round_down_to_bucket_start(timestamp, duration);

        let duration_be = duration.encode_be();
        let bucket_time_millis_be = bucket_time_millis.encode_be();

        let hash = hashing::hash_multiple(&[&[bucket_type as u8], base_id.as_ref(), duration_be.as_ref(), bucket_time_millis_be.as_ref()]);
        let location_id = Id::from_hash(hash)?;

        Ok(BucketLocation { bucket_type, base_id, duration, bucket_time_millis, location_id })
    }
    pub fn get_hash_for_signing(&self) -> crate::tools::types::Hash {
        let bucket_type_byte = [self.bucket_type as u8];
        let duration_be = self.duration.encode_be();
        let bucket_time_be = self.bucket_time_millis.encode_be();
        hashing::hash_multiple(&[&bucket_type_byte, self.base_id.as_ref(), duration_be.as_ref(), bucket_time_be.as_ref()])
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        let other = Self::new(self.bucket_type, self.base_id, self.duration, self.bucket_time_millis)?;
        if self.location_id != other.location_id {
            anyhow::bail!("BucketLocation validation failed");
        }
        Ok(())
    }

    /// Serialise to a versioned, tilde-delimited string safe for use as an HTML attribute value or URL segment.
    /// Format: `{version}~{bucket_type}~{base_id_hex}~{duration}~{bucket_time_millis}`
    /// Example: `1~User~aabb...cc~1D~20240115.123045.000`
    /// `location_id` is omitted — it is recomputed from the other fields on deserialisation.
    /// Future versions may append additional tilde-separated fields; readers must ignore unknown fields.
    pub fn to_html_attr(&self) -> String {
        format!("1~{}~{}~{}~{}", self.bucket_type, self.base_id.to_hex_str(), self.duration, self.bucket_time_millis)
    }

    /// Deserialise from the format produced by [`BucketLocation::to_html_attr`].
    pub fn from_html_attr(s: &str) -> anyhow::Result<Self> {
        let parts: Vec<&str> = s.splitn(6, '~').collect();
        anyhow::ensure!(parts.len() >= 5, "Invalid BucketLocation attr: expected at least 5 tilde-separated parts");
        anyhow::ensure!(parts[0] == "1", "Unsupported BucketLocation attr version: {}", parts[0]);

        let bucket_type = match parts[1] {
            "User" => BucketType::User,
            "Hashtag" => BucketType::Hashtag,
            "Mention" => BucketType::Mention,
            "ReplyToPost" => BucketType::ReplyToPost,
            "Sequel" => BucketType::Sequel,
            other => anyhow::bail!("Unknown BucketType: {}", other),
        };
        let base_id = Id::from_hex_str(parts[2])?;
        let duration = DurationMillis::parse(parts[3])?;
        let bucket_time_millis = TimeMillis::parse(parts[4])?;

        Self::new(bucket_type, base_id, duration, bucket_time_millis)
    }
}
pub fn generate_bucket_location(bucket_type: BucketType, base_id: Id, bucket_duration: DurationMillis, time_millis: TimeMillis) -> anyhow::Result<BucketLocation> {
        BucketLocation::new(bucket_type, base_id, bucket_duration, time_millis)
    }

/// These are the durations of the hierarchical buckets that posts will collect in.
/// They are spaced such that there are always roughly 4-6 "more granular" buckets per "less granular" bucket.
/// This ensures that no overflowed bucket would result in many tiny buckets needing to be scanned at the next level.
/// Also make sure that the "less granular" bucket is a round multiple of its "more` granular" successor.
pub const BUCKET_DURATIONS: [DurationMillis; 9] = [MILLIS_IN_YEAR, MILLIS_IN_MONTH, MILLIS_IN_WEEK, MILLIS_IN_DAY, MILLIS_IN_HOUR.const_mul(6), MILLIS_IN_HOUR, MILLIS_IN_MINUTE.const_mul(15), MILLIS_IN_MINUTE.const_mul(5), MILLIS_IN_MINUTE];

/// Returns the appropriate bucket duration slice for a given bucket type.
/// Sequel buckets start at year granularity for cheaper discoverability.
/// All other bucket types start at month granularity so that a single location_id (set of servers) is not responsible for a year of potentially high activity.
pub fn bucket_durations_for_type(bucket_type: BucketType) -> &'static [DurationMillis] {
    match bucket_type {
        BucketType::Sequel => &BUCKET_DURATIONS,       // starts at year
        _ => &BUCKET_DURATIONS[1..],                    // starts at month
    }
}


#[cfg(test)]
pub mod tests {
    use crate::tools::buckets::{bucket_durations_for_type, BucketLocation, BucketType, BUCKET_DURATIONS};
    use crate::tools::time::{TimeMillis, MILLIS_IN_DAY, MILLIS_IN_MONTH, MILLIS_IN_YEAR};
    use crate::tools::types::Id;

    #[tokio::test]
    async fn ensure_bucket_duration_multiples_test() -> anyhow::Result<()> {
        for i in 0..BUCKET_DURATIONS.len()-1 {
            assert_eq!(0, BUCKET_DURATIONS[i].0 % BUCKET_DURATIONS[i+1].0)
        }
        Ok(())
    }

    #[tokio::test]
    async fn bucket_location_html_attr_round_trip() -> anyhow::Result<()> {
        let base_id = Id::random();
        let original = BucketLocation::new(BucketType::Hashtag, base_id, MILLIS_IN_DAY, TimeMillis(1_700_000_000_000))?;
        let attr = original.to_html_attr();
        let restored = BucketLocation::from_html_attr(&attr)?;
        assert_eq!(original, restored);
        // Sanity: attr starts with version prefix
        assert!(attr.starts_with("1~"));
        Ok(())
    }

    #[tokio::test]
    async fn bucket_location_sequel_html_attr_round_trip() -> anyhow::Result<()> {
        let base_id = Id::random();
        let original = BucketLocation::new(BucketType::Sequel, base_id, MILLIS_IN_YEAR, TimeMillis(1_700_000_000_000))?;
        let attr = original.to_html_attr();
        let restored = BucketLocation::from_html_attr(&attr)?;
        assert_eq!(original, restored);
        assert!(attr.contains("Sequel"));
        Ok(())
    }

    #[tokio::test]
    async fn bucket_durations_for_type_sequel_starts_at_year() -> anyhow::Result<()> {
        let sequel_durations = bucket_durations_for_type(BucketType::Sequel);
        let user_durations = bucket_durations_for_type(BucketType::User);
        assert_eq!(sequel_durations[0], MILLIS_IN_YEAR);
        assert_eq!(user_durations[0], MILLIS_IN_MONTH);
        assert_eq!(sequel_durations.len(), user_durations.len() + 1);
        Ok(())
    }
}
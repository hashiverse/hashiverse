//! # Wire-format time and duration types
//!
//! Two newtypes that replace raw `i64` / `Duration` throughout the protocol:
//!
//! - [`TimeMillis`] — an instant as milliseconds since the Unix epoch. Used on every
//!   signed message (post timestamps, peer announcements, PoW birth certificates) so
//!   that clock drift can be bounded by `POW_MAX_CLOCK_DRIFT_MILLIS`.
//! - [`DurationMillis`] — a span of milliseconds. Used for bucket durations, cache
//!   TTLs, retry windows. Supports `const_mul` so the constants in
//!   [`crate::tools::config`] can be composed at compile time.
//!
//! Both types round-trip through a fixed 8-byte big-endian wire encoding
//! ([`TimeMillisBytes`] and [`DurationMillisBytes`]) so they hash and sign the same
//! way on every platform. Human-readable parsing / formatting uses a
//! `YYYYMMDD.hhmmss.SSS` format for times and `<N><unit>` for durations (e.g. `1D`,
//! `15m`, `250M` for milliseconds).
//!
//! The canonical unit constants — [`MILLIS_IN_MILLISECOND`], [`MILLIS_IN_SECOND`], …,
//! [`MILLIS_IN_YEAR`] — are `const DurationMillis` so they can be used in `const`
//! contexts (e.g. `pub const X: DurationMillis = MILLIS_IN_MINUTE.const_mul(5);`).

use crate::tools::tools;
use chrono::{Datelike, TimeZone, Timelike, Utc};
use derive_more::{Add, Div, Mul, Neg, Sub};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{Add, Mul, Sub, SubAssign};
use std::time::Duration;

pub const TIME_MILLIS_BYTES: usize = size_of::<TimeMillis>();
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct TimeMillisBytes(pub [u8; TIME_MILLIS_BYTES]);

impl TimeMillisBytes {
    pub(crate) fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }

    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        if bytes.len() != TIME_MILLIS_BYTES {
            anyhow::bail!("Invalid time millis bytes length: {}", bytes.len());
        }

        Ok(Self(bytes.try_into()?))
    }
}

#[derive(Ord, PartialOrd, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimeMillis(pub i64);

impl TimeMillis {
    pub fn zero() -> Self {
        Self(0)
    }
    pub const MAX: Self = Self(i64::MAX);
    pub const MIN: Self = Self(i64::MIN);

    pub fn random() -> TimeMillis {
        TimeMillis(tools::random_u32() as i64)
    }

    pub fn saturating_add(self, other: DurationMillis) -> TimeMillis {
        TimeMillis(self.0.saturating_add(other.0))
    }

    pub fn saturating_sub_duration(self, other: DurationMillis) -> TimeMillis {
        TimeMillis(self.0.saturating_sub(other.0))
    }
    pub fn saturating_sub_time(self, other: TimeMillis) -> DurationMillis {
        DurationMillis(self.0.saturating_sub(other.0))
    }

    pub fn as_secs(&self) -> i64 {
        self.0 / 1000
    }

    pub fn part_nanos(&self) -> i64 {
        (self.0 % 1000) * 1_000_000
    }

    pub fn encode_be(self) -> TimeMillisBytes {
        TimeMillisBytes(i64::to_be_bytes(self.0))
    }
    pub fn timestamp_decode_be(timestamp_bytes: &TimeMillisBytes) -> Self {
        let time_millis = i64::from_be_bytes(timestamp_bytes.0);
        Self(time_millis)
    }

    pub fn from_epoch_offset_str(duration_millis_str: &str) -> anyhow::Result<Self> {
        Ok(Self::zero() + DurationMillis::parse(duration_millis_str)?)
    }

    /// Parse a `TimeMillis` from the format produced by its `Display` impl: `YYYYMMDD.HHMMSS.mmm` (UTC).
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        let parts: Vec<&str> = s.split('.').collect();
        anyhow::ensure!(parts.len() == 3 && parts[0].len() >= 8 && parts[1].len() == 6 && parts[2].len() == 3,
            "Invalid TimeMillis string (expected YYYYMMDD.HHMMSS.mmm): {:?}", s);

        let year:   i32 = parts[0][..parts[0].len()-4].parse()?;
        let month:  u32 = parts[0][parts[0].len()-4..parts[0].len()-2].parse()?;
        let day:    u32 = parts[0][parts[0].len()-2..].parse()?;
        let hour:   u32 = parts[1][0..2].parse()?;
        let minute: u32 = parts[1][2..4].parse()?;
        let second: u32 = parts[1][4..6].parse()?;
        let millis: i64 = parts[2].parse()?;

        let dt = Utc.with_ymd_and_hms(year, month, day, hour, minute, second)
            .single()
            .ok_or_else(|| anyhow::anyhow!("Invalid UTC date/time in TimeMillis string: {:?}", s))?;

        Ok(Self(dt.timestamp() * 1000 + millis))
    }
}

impl fmt::Display for TimeMillis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let millis = self.0;
        let secs = millis / 1000;
        let millis_part = millis % 1000;

        // Convert Unix timestamp to DateTime using chrono
        let dt = Utc.timestamp_opt(secs, 0);
        match dt {
            chrono::LocalResult::Single(dt) => write!(f, "{}{:02}{:02}.{:02}{:02}{:02}.{:03}", dt.year(), dt.month(), dt.day(), dt.hour(), dt.minute(), dt.second(), millis_part),
            chrono::LocalResult::Ambiguous(_dt_earlier, dt) => write!(f, "{}{:02}{:02}.{:02}{:02}{:02}.{:03} (ambiguous)", dt.year(), dt.month(), dt.day(), dt.hour(), dt.minute(), dt.second(), millis_part),
            _ => write!(f, "<invalid-timestamp:{}>", self.0),
        }
    }
}

impl fmt::Debug for TimeMillis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl Sub<TimeMillis> for TimeMillis {
    type Output = DurationMillis;

    fn sub(self, rhs: TimeMillis) -> Self::Output {
        DurationMillis(self.0.saturating_sub(rhs.0))
    }
}

impl SubAssign<DurationMillis> for TimeMillis {
    fn sub_assign(&mut self, rhs: DurationMillis) {
        self.0 = self.0.saturating_sub(rhs.0);
    }
}

impl Add<DurationMillis> for TimeMillis {
    type Output = TimeMillis;

    fn add(self, rhs: DurationMillis) -> Self::Output {
        Self(self.0.saturating_add(rhs.0))
    }
}

impl Sub<DurationMillis> for TimeMillis {
    type Output = TimeMillis;

    fn sub(self, rhs: DurationMillis) -> Self::Output {
        Self(self.0.saturating_sub(rhs.0))
    }
}

impl Add<Duration> for TimeMillis {
    type Output = TimeMillis;

    fn add(self, rhs: Duration) -> Self::Output {
        Self(self.0.saturating_add(rhs.as_millis() as i64))
    }
}
impl Mul<Duration> for TimeMillis {
    type Output = TimeMillis;

    fn mul(self, rhs: Duration) -> Self::Output {
        Self(self.0.saturating_mul(rhs.as_millis() as i64))
    }
}

impl From<std::time::SystemTime> for TimeMillis {
    fn from(time: std::time::SystemTime) -> Self {
        TimeMillis(time.duration_since(std::time::SystemTime::UNIX_EPOCH).unwrap_or_default().as_millis() as i64)
    }
}

pub const DURATION_MILLIS_BYTES: usize = size_of::<DurationMillis>();
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DurationMillisBytes(pub [u8; DURATION_MILLIS_BYTES]);

impl DurationMillisBytes {
    pub(crate) fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }

    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        if bytes.len() != DURATION_MILLIS_BYTES {
            anyhow::bail!("Invalid duration millis bytes length: {}", bytes.len());
        }

        Ok(Self(bytes.try_into()?))
    }
}

#[derive(Ord, PartialOrd, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Add, Sub, Neg, Mul, Div)]
pub struct DurationMillis(pub i64);

impl DurationMillis {
    pub fn zero() -> Self {
        Self(0)
    }
    pub fn encode_be(self) -> DurationMillisBytes {
        DurationMillisBytes(i64::to_be_bytes(self.0))
    }

    // Just waiting the day that the Mul trait is marked as const...
    pub const fn const_add(self, rhs: DurationMillis) -> DurationMillis {
        DurationMillis(self.0 + rhs.0)
    }
    pub const fn const_mul(&self, rhs: i64) -> DurationMillis {
        Self(self.0 * rhs)
    }

    pub const fn abs(self) -> DurationMillis {
        Self(self.0.abs())
    }

    /// Parse a duration formatted the same way as `DurationMillis::fmt` / `Display`.
    ///
    /// Examples: `"1M"`, `"3h27m"`, `"1D3h7m"`, `""` (zero), `"-5m"`.
    /// Units supported (must match formatter): `M W D h m s μs`
    pub fn parse(input: &str) -> anyhow::Result<Self> {
        let s = input.trim();
        if s.is_empty() {
            return Ok(DurationMillis::zero());
        }
        if s == "0" || s == "+0" || s == "-0" {
            return Ok(DurationMillis::zero());
        }

        let mut idx = 0usize;
        let bytes = s.as_bytes();

        let mut sign: i64 = 1;
        if bytes[0] == b'+' {
            idx += 1;
        } else if bytes[0] == b'-' {
            sign = -1;
            idx += 1;
        }

        if idx >= s.len() {
            anyhow::bail!("Invalid duration: {:?}", input);
        }

        let mut total: i64 = 0;
        let mut saw_any = false;

        while idx < s.len() {
            // parse number
            let num_start = idx;
            while idx < s.len() {
                let c = bytes[idx];

                if c.is_ascii_digit() {
                    idx += 1;
                } else {
                    break;
                }
            }
            if idx == num_start {
                anyhow::bail!("Invalid duration: expected number at {:?}", &s[idx..]);
            }

            let n: i64 = s[num_start..idx]
                .parse()
                .map_err(|e| anyhow::anyhow!("Invalid duration number {:?}: {}", &s[num_start..idx], e))?;

            if idx >= s.len() {
                anyhow::bail!("Invalid duration: missing unit after {}", n);
            }

            // parse unit (μs is 2 bytes in UTF-8, so handle as a string prefix)
            let rest = &s[idx..];
            let (unit_millis, unit_len) = if rest.starts_with("μs") {
                (MILLIS_IN_MILLISECOND.0, "μs".len())
            } else if rest.starts_with('M') {
                (MILLIS_IN_MONTH.0, 1)
            } else if rest.starts_with('W') {
                (MILLIS_IN_WEEK.0, 1)
            } else if rest.starts_with('D') {
                (MILLIS_IN_DAY.0, 1)
            } else if rest.starts_with('h') {
                (MILLIS_IN_HOUR.0, 1)
            } else if rest.starts_with('m') {
                (MILLIS_IN_MINUTE.0, 1)
            } else if rest.starts_with('s') {
                (MILLIS_IN_SECOND.0, 1)
            } else {
                anyhow::bail!("Invalid duration: unknown unit at {:?}", rest);
            };

            // advance by unit length (in bytes)
            idx += unit_len;

            // accumulate, guarding against overflow
            let chunk = n
                .checked_mul(unit_millis)
                .ok_or_else(|| anyhow::anyhow!("Duration overflow"))?;
            total = total
                .checked_add(chunk)
                .ok_or_else(|| anyhow::anyhow!("Duration overflow"))?;

            saw_any = true;
        }

        if !saw_any {
            anyhow::bail!("Invalid duration: {:?}", input);
        }

        Ok(DurationMillis(
            total
                .checked_mul(sign)
                .ok_or_else(|| anyhow::anyhow!("Duration overflow"))?,
        ))
    }
}

pub const MILLIS_IN_MILLISECOND: DurationMillis = DurationMillis(1);
pub const MILLIS_IN_SECOND: DurationMillis = MILLIS_IN_MILLISECOND.const_mul(1000);
pub const MILLIS_IN_MINUTE: DurationMillis = MILLIS_IN_SECOND.const_mul(60);
pub const MILLIS_IN_HOUR: DurationMillis = MILLIS_IN_MINUTE.const_mul(60);
pub const MILLIS_IN_DAY: DurationMillis = MILLIS_IN_HOUR.const_mul(24);
pub const MILLIS_IN_WEEK: DurationMillis = MILLIS_IN_DAY.const_mul(7);
pub const MILLIS_IN_MONTH: DurationMillis = MILLIS_IN_WEEK.const_mul(4); // NB - this HAS be a multiple of weeks so that we have good behaviour of bucket recursion
pub const MILLIS_IN_YEAR: DurationMillis = MILLIS_IN_MONTH.const_mul(12); // NB - this HAS to be a multiple of months so that we have good behaviour of bucket recursion

impl fmt::Display for DurationMillis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = String::with_capacity(32);

        if self.0 < 0 {
            s.push('-');
        }

        let mut remaining = self.0.saturating_abs();

        let mut do_unit = |span: DurationMillis, descr: &str| {
            if remaining >= span.0 {
                let units = remaining / span.0;
                remaining -= units * span.0;
                s.push_str(&format!("{}{}", units, descr))
            }
        };

        do_unit(MILLIS_IN_MONTH, "M");
        do_unit(MILLIS_IN_WEEK, "W");
        do_unit(MILLIS_IN_DAY, "D");
        do_unit(MILLIS_IN_HOUR, "h");
        do_unit(MILLIS_IN_MINUTE, "m");
        do_unit(MILLIS_IN_SECOND, "s");
        do_unit(MILLIS_IN_MILLISECOND, "μs");  // Not microseconds, i know, but for parsing i accept this hack

        if s.is_empty() || s == "-" {
            s = "0".to_string()
        }

        write!(f, "{}", s)
    }
}

impl fmt::Debug for DurationMillis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}


impl From<DurationMillis> for Duration {
    fn from(value: DurationMillis) -> Self {
        if value.0 <= 0 {
            Duration::ZERO
        } else {
            Duration::from_millis(value.0 as u64)
        }
    }
}

pub fn to_bucket(timestamp: &TimeMillis, bucket_size: DurationMillis) -> TimeMillis {
    TimeMillis((timestamp.0 / bucket_size.0) * bucket_size.0)
}

#[cfg(test)]
mod tests {
    use crate::tools::time::{MILLIS_IN_HOUR, MILLIS_IN_MINUTE, MILLIS_IN_MONTH, DurationMillis, MILLIS_IN_SECOND, MILLIS_IN_WEEK, MILLIS_IN_DAY, TimeMillis};

    #[tokio::test]
    async fn duration_display_test() {
        assert_eq!(MILLIS_IN_MONTH.to_string(), "1M");
        assert_eq!(MILLIS_IN_HOUR.to_string(), "1h");
        assert_eq!((MILLIS_IN_HOUR.const_mul(3) + MILLIS_IN_MINUTE.const_mul(27)).to_string(), "3h27m");
        assert_eq!((MILLIS_IN_HOUR.const_mul(26) + MILLIS_IN_MINUTE.const_mul(67)).to_string(), "1D3h7m");
        // Negative durations
        assert_eq!((-MILLIS_IN_SECOND.const_mul(5)).to_string(), "-5s");
        assert_eq!((-MILLIS_IN_MINUTE.const_mul(2) + -MILLIS_IN_SECOND.const_mul(5)).to_string(), "-2m5s");
        assert_eq!(DurationMillis::zero().to_string(), "0");
    }

    #[test]
    fn duration_parse_roundtrip_examples() -> anyhow::Result<()> {
        let d1 = MILLIS_IN_WEEK.const_mul(3) + MILLIS_IN_DAY.const_mul(3);
        assert_eq!(DurationMillis::parse(&d1.to_string())?, d1);

        let d2 = MILLIS_IN_WEEK.const_mul(2) + MILLIS_IN_DAY.const_mul(5);
        assert_eq!(DurationMillis::parse(&d2.to_string())?, d2);

        assert_eq!(DurationMillis::parse("")?, DurationMillis::zero());
        assert_eq!(DurationMillis::parse("0")?, DurationMillis::zero());

        Ok(())
    }

    #[test]
    fn duration_parse_negative() -> anyhow::Result<()> {
        let d = -(MILLIS_IN_SECOND.const_mul(5) + MILLIS_IN_MINUTE.const_mul(2));
        assert_eq!(DurationMillis::parse("-2m5s")?, d);
        Ok(())
    }

    #[test]
    fn time_millis_display() {
        // Known UTC timestamps verified against calendar
        assert_eq!(TimeMillis(0).to_string(),                    "19700101.000000.000"); // Unix epoch
        assert_eq!(TimeMillis(1_700_000_000_000).to_string(),    "20231114.221320.000"); // 2023-11-14 22:13:20 UTC
        assert_eq!(TimeMillis(1_672_531_200_000).to_string(),    "20230101.000000.000"); // 2023-01-01 00:00:00 UTC
        assert_eq!(TimeMillis(1_700_000_000_123).to_string(),    "20231114.221320.123"); // non-zero millis
        assert_eq!(TimeMillis(1_700_000_000_999).to_string(),    "20231114.221320.999"); // millis = 999
    }

    #[test]
    fn time_millis_parse_roundtrip() -> anyhow::Result<()> {
        let cases = [
            TimeMillis(0),                    // Unix epoch
            TimeMillis(1_700_000_000_000),    // arbitrary recent timestamp
            TimeMillis(1_672_531_200_000),    // start of 2023
            TimeMillis(1_700_000_000_123),    // non-zero millis part
            TimeMillis(1_700_000_000_999),    // millis = 999
        ];
        for t in cases {
            let s = t.to_string();
            let parsed = TimeMillis::parse(&s)?;
            assert_eq!(t, parsed, "round-trip failed for {} (string: {:?})", t.0, s);
        }
        Ok(())
    }

    #[test]
    fn time_millis_parse_known_strings() -> anyhow::Result<()> {
        assert_eq!(TimeMillis::parse("19700101.000000.000")?, TimeMillis(0));
        assert_eq!(TimeMillis::parse("20231114.221320.000")?, TimeMillis(1_700_000_000_000));
        assert_eq!(TimeMillis::parse("20231114.221320.123")?, TimeMillis(1_700_000_000_123));
        Ok(())
    }

    #[test]
    fn time_millis_parse_errors() {
        assert!(TimeMillis::parse("").is_err());
        assert!(TimeMillis::parse("20231114.221320").is_err());       // missing millis part
        assert!(TimeMillis::parse("20231114.221320.1234").is_err());  // millis too long
        assert!(TimeMillis::parse("20231114.22132x.000").is_err());   // non-numeric
        assert!(TimeMillis::parse("20231332.000000.000").is_err());   // invalid day 32
    }

    #[test]
    fn time_millis_arithmetic_saturates_on_overflow() {
        // Sub<TimeMillis>: MIN - MAX should saturate, not panic
        assert_eq!(TimeMillis::MIN - TimeMillis::MAX, DurationMillis(i64::MIN));
        assert_eq!(TimeMillis(0) - TimeMillis(1), DurationMillis(-1));
        assert_eq!(TimeMillis::MAX - TimeMillis::MIN, DurationMillis(i64::MAX));

        // Sub<DurationMillis>: subtracting a huge positive duration from MIN
        assert_eq!(TimeMillis::MIN - DurationMillis(i64::MAX), TimeMillis::MIN);
        assert_eq!(TimeMillis(0) - DurationMillis(1), TimeMillis(-1));

        // Add<DurationMillis>: adding a huge positive duration to MAX
        assert_eq!(TimeMillis::MAX + DurationMillis(1), TimeMillis::MAX);
        assert_eq!(TimeMillis::MIN + DurationMillis(-1), TimeMillis::MIN);

        // SubAssign<DurationMillis>
        let mut time = TimeMillis::MIN;
        time -= DurationMillis(1);
        assert_eq!(time, TimeMillis::MIN);
    }
}

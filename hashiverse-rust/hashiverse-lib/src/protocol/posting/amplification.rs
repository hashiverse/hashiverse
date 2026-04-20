//! # Post PoW amplification rules
//!
//! A single function — [`get_minimum_post_pow`] — that decides how many bits of
//! proof-of-work a post requires before a server will accept it. The base level comes
//! from [`crate::tools::config::POW_MINIMUM_PER_POST`], on top of which the
//! amplification is:
//!
//! - **Size**: log-scale above 1024 bytes. Longer posts cost more — fair for storage,
//!   punishes spam-generated wall-of-text content.
//! - **Fan-out**: posts that link a large number of `base_id`s (hashtags, mentions,
//!   replies) scale quadratically. Main lever against hashtag spam farms that link
//!   hundreds of trending tags from one post.
//! - **Bucket granularity**: submissions into tighter (shorter-duration) buckets need
//!   more PoW than broader ones. Hot, fine-grained buckets are the most valuable
//!   attack surface, so they get the most expensive admission control.

use crate::tools::config;
use crate::tools::time::{DurationMillis, MILLIS_IN_MINUTE, MILLIS_IN_MONTH};
use crate::tools::types::Pow;

pub fn get_minimum_post_pow(post_length: usize, linked_base_ids_len: usize, duration: DurationMillis) -> Pow {
    assert!(duration > MILLIS_IN_MINUTE);

    let pow_amplification_post_length = if post_length <= 1024 { 1.0f64 } else { 1.0f64 + 1.75 * (post_length as f64 / 1024f64).log2() };
    let pow_amplification_linked_base_ids = if linked_base_ids_len < 2 {1.0f64} else { 1.0f64 + 0.2f64 * (linked_base_ids_len as f64).powi(2) };
    let pow_amplification_bucket = 1.0f64 + (MILLIS_IN_MONTH.0 as f64 / duration.0 as f64).log2();

    let pow_amplification = pow_amplification_post_length * pow_amplification_linked_base_ids * pow_amplification_bucket;
    let additional_pow_bits = pow_amplification.log2().ceil();
    Pow(config::POW_MINIMUM_PER_POST.0 + additional_pow_bits as u8)
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::config;
    use crate::tools::time::{MILLIS_IN_DAY, MILLIS_IN_HOUR, MILLIS_IN_MONTH};
    use crate::tools::types::Pow;

    // baseline: small post, client_id only (< 2 links → no extra amp), month → total=1.0, ceil to +0 bits
    #[test]
    fn baseline() {
        let pow = get_minimum_post_pow(512, 1, MILLIS_IN_MONTH);
        assert_eq!(pow, config::POW_MINIMUM_PER_POST);
    }

    // day bucket: bucket_amp≈5.81, linked_amp=1.0 → total=5.81, log2≈2.54 → ceil to +3 bits
    #[test]
    fn day_bucket() {
        let pow = get_minimum_post_pow(512, 1, MILLIS_IN_DAY);
        assert_eq!(pow, Pow(config::POW_MINIMUM_PER_POST.0 + 3));
    }

    // hour bucket: bucket_amp≈10.39, linked_amp=1.0 → total=10.39, log2≈3.38 → ceil to +4 bits
    #[test]
    fn hour_bucket() {
        let pow = get_minimum_post_pow(512, 1, MILLIS_IN_HOUR);
        assert_eq!(pow, Pow(config::POW_MINIMUM_PER_POST.0 + 4));
    }

    // linked ids (3): amplification kicks in at ≥ 2 links; linked_amp = 1 + 0.2*9 = 2.8, log2≈1.49 → ceil to +2 bits
    #[test]
    fn linked_ids() {
        let pow = get_minimum_post_pow(512, 3, MILLIS_IN_MONTH);
        assert_eq!(pow, Pow(config::POW_MINIMUM_PER_POST.0 + 2));
    }

    // combined: post_amp=4.5, linked_amp=2.8, bucket_amp≈5.81 → product≈73.2, log2≈6.19 → ceil to +7 bits
    #[test]
    fn combined() {
        let pow = get_minimum_post_pow(4096, 3, MILLIS_IN_DAY);
        assert_eq!(pow, Pow(config::POW_MINIMUM_PER_POST.0 + 7));
    }

    // medium post (4 KB): post_amp=4.5, linked_amp=1.0 → total=4.5, log2≈2.17 → ceil to +3 bits
    #[test]
    fn medium_post_4kb() {
        let pow = get_minimum_post_pow(4 * 1024, 1, MILLIS_IN_MONTH);
        assert_eq!(pow, Pow(config::POW_MINIMUM_PER_POST.0 + 3));
    }

    // large post (64 KB): post_amp=11.5, linked_amp=1.0 → total=11.5, log2≈3.52 → ceil to +4 bits
    #[test]
    fn large_post_64kb() {
        let pow = get_minimum_post_pow(64 * 1024, 1, MILLIS_IN_MONTH);
        assert_eq!(pow, Pow(config::POW_MINIMUM_PER_POST.0 + 4));
    }

    // large post (512 KB): post_amp=16.75, linked_amp=1.0 → total=16.75, log2≈4.07 → ceil to +5 bits
    #[test]
    fn large_post_512kb() {
        let pow = get_minimum_post_pow(512 * 1024, 1, MILLIS_IN_MONTH);
        assert_eq!(pow, Pow(config::POW_MINIMUM_PER_POST.0 + 5));
    }

    // minute-granularity bucket (5 min): bucket_amp≈13.977, linked_amp=1.0 → total=13.977, log2≈3.80 → ceil to +4 bits
    #[test]
    fn minute_granularity_5min() {
        let pow = get_minimum_post_pow(512, 1, MILLIS_IN_MINUTE.const_mul(5));
        assert_eq!(pow, Pow(config::POW_MINIMUM_PER_POST.0 + 4));
    }

    // many linked ids (10): linked_amp = 1 + 0.2*100 = 21.0, total log2 ≈ 4.39 → ceil to +5 bits
    #[test]
    fn many_linked_ids_10() {
        let pow = get_minimum_post_pow(512, 10, MILLIS_IN_MONTH);
        assert_eq!(pow, Pow(config::POW_MINIMUM_PER_POST.0 + 5));
    }

    // shorter duration → higher or equal pow
    #[test]
    fn monotone_duration() {
        let pow_month = get_minimum_post_pow(512, 1, MILLIS_IN_MONTH);
        let pow_day = get_minimum_post_pow(512, 1, MILLIS_IN_DAY);
        let pow_hour = get_minimum_post_pow(512, 1, MILLIS_IN_HOUR);
        assert!(pow_month <= pow_day);
        assert!(pow_day <= pow_hour);
        assert!(pow_month < pow_hour);
    }

    // more linked ids → higher or equal pow (minimum real-world value is 1 — the client_id)
    #[test]
    fn monotone_linked_ids() {
        let pow_1 = get_minimum_post_pow(512, 1, MILLIS_IN_DAY);
        let pow_2 = get_minimum_post_pow(512, 2, MILLIS_IN_DAY);
        let pow_3 = get_minimum_post_pow(512, 3, MILLIS_IN_DAY);
        let pow_4 = get_minimum_post_pow(512, 4, MILLIS_IN_DAY);
        assert!(pow_1 <= pow_2);
        assert!(pow_2 <= pow_3);
        assert!(pow_3 <= pow_4);
        assert!(pow_1 < pow_4);
    }

    // larger posts → higher or equal pow (only kicks in above 1024 bytes)
    #[test]
    fn monotone_post_length() {
        let pow_small = get_minimum_post_pow(1024, 1, MILLIS_IN_DAY);
        let pow_medium = get_minimum_post_pow(8192, 1, MILLIS_IN_DAY);
        let pow_large = get_minimum_post_pow(1024 * 1024, 1, MILLIS_IN_DAY);
        assert!(pow_small <= pow_medium);
        assert!(pow_medium <= pow_large);
        assert!(pow_small < pow_large);
    }

    // every combination on a sample grid must be at or above the minimum
    #[test]
    fn never_below_minimum() {
        let lengths = [1, 512, 1024, 4096, 64 * 1024];
        let links = [1, 2, 5, 10];
        let durations = [MILLIS_IN_HOUR, MILLIS_IN_DAY, MILLIS_IN_MONTH];
        for post_length in lengths {
            for linked_base_ids_len in links {
                for duration in durations {
                    let pow = get_minimum_post_pow(post_length, linked_base_ids_len, duration);
                    assert!(
                        pow >= config::POW_MINIMUM_PER_POST,
                        "pow={:?} below minimum for post_length={} links={} duration={:?}",
                        pow, post_length, linked_base_ids_len, duration
                    );
                }
            }
        }
    }

    // durations shorter than a minute are not valid bucket lengths and must panic
    #[test]
    #[should_panic]
    fn panic_on_short_duration() {
        get_minimum_post_pow(512, 1, MILLIS_IN_MINUTE);
    }
}
//! # Compact probabilistic cardinality estimation
//!
//! A HyperLogLog sketch backed by Blake3 hashing. Used where we need an approximate count
//! of distinct items (for example, unique viewers of a post or unique voters on a piece of
//! feedback) without storing the items themselves — the sketch is a fixed 64 bytes
//! regardless of how many distinct inputs have been seen.
//!
//! Configured with 2^6 = 64 registers (yielding ~7.8% standard error), which keeps the
//! sketch small enough to embed freely in RPC responses and on-disk records while still
//! being accurate enough for UI-grade counts.

use crate::tools::hashing;

/// A HyperLogLog probabilistic cardinality estimator.
///
/// Uses 64 registers (2^6), giving ~7.8% standard error.
/// Backed by Blake3 hashing.
const NUM_REGISTERS_POWER: u8 = 6;
const NUM_REGISTERS: usize = 1 << NUM_REGISTERS_POWER; // 64

/// Alpha constant for bias correction with m=64
const ALPHA: f64 = 0.709;

#[derive(Clone, Debug)]
pub struct HyperLogLog {
    registers: [u8; NUM_REGISTERS],
}

impl HyperLogLog {
    pub fn new() -> Self {
        Self { registers: [0; NUM_REGISTERS] }
    }

    pub fn insert(&mut self, data: &[u8]) {
        let hash = hashing::hash(data);
        let hash_bytes = hash.0;

        // Use first byte bits for register index (we need 6 bits for 64 registers)
        let register_index = (hash_bytes[0] as usize) & (NUM_REGISTERS - 1);

        // Count leading zeros in the remaining bits (starting from bit 6 onward).
        // We reconstruct a u64 from bytes 1..9 for the leading-zero count.
        let remaining_value = u64::from_be_bytes([
            hash_bytes[1], hash_bytes[2], hash_bytes[3], hash_bytes[4],
            hash_bytes[5], hash_bytes[6], hash_bytes[7], hash_bytes[8],
        ]);

        // leading_zeros + 1 (HLL convention: rho function)
        let leading_zeros_plus_one = (remaining_value.leading_zeros() + 1) as u8;

        if leading_zeros_plus_one > self.registers[register_index] {
            self.registers[register_index] = leading_zeros_plus_one;
        }
    }

    pub fn count(&self) -> u64 {
        let num_registers_f64 = NUM_REGISTERS as f64;

        // Harmonic mean of 2^(-register[i])
        let harmonic_sum: f64 = self.registers.iter().map(|&register_value| {
            2.0_f64.powi(-(register_value as i32))
        }).sum();

        let raw_estimate = ALPHA * num_registers_f64 * num_registers_f64 / harmonic_sum;

        // Small range correction: use linear counting if estimate is small and there are zero registers
        if raw_estimate <= 2.5 * num_registers_f64 {
            let num_zero_registers = self.registers.iter().filter(|&&v| v == 0).count();
            if num_zero_registers > 0 {
                // Linear counting
                let linear_count = num_registers_f64 * (num_registers_f64 / num_zero_registers as f64).ln();
                return linear_count as u64;
            }
        }

        raw_estimate as u64
    }

    pub fn merge(&mut self, other: &HyperLogLog) {
        for i in 0..NUM_REGISTERS {
            if other.registers[i] > self.registers[i] {
                self.registers[i] = other.registers[i];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hyperloglog_empty() {
        let hll = HyperLogLog::new();
        assert_eq!(hll.count(), 0);
    }

    #[test]
    fn test_hyperloglog_single_insert() {
        let mut hll = HyperLogLog::new();
        hll.insert(b"hello");
        assert!(hll.count() >= 1);
    }

    #[test]
    fn test_hyperloglog_duplicate_inserts() {
        let mut hll = HyperLogLog::new();
        for _ in 0..1000 {
            hll.insert(b"same_value");
        }
        // Duplicates should not inflate the count
        assert_eq!(hll.count(), 1);
    }

    #[test]
    fn test_hyperloglog_distinct_values() {
        let mut hll = HyperLogLog::new();
        let num_distinct = 1000;
        for i in 0..num_distinct {
            hll.insert(format!("value_{}", i).as_bytes());
        }
        let estimated_count = hll.count();
        // With 64 registers (~7.8% standard error), allow 25% tolerance for test stability
        let lower_bound = (num_distinct as f64 * 0.75) as u64;
        let upper_bound = (num_distinct as f64 * 1.25) as u64;
        assert!(
            estimated_count >= lower_bound && estimated_count <= upper_bound,
            "Expected count near {}, got {} (bounds: {}-{})", num_distinct, estimated_count, lower_bound, upper_bound
        );
    }

    #[test]
    fn test_hyperloglog_merge() {
        let mut hll_a = HyperLogLog::new();
        let mut hll_b = HyperLogLog::new();

        for i in 0..500 {
            hll_a.insert(format!("a_{}", i).as_bytes());
        }
        for i in 0..500 {
            hll_b.insert(format!("b_{}", i).as_bytes());
        }

        let count_a = hll_a.count();
        let count_b = hll_b.count();

        hll_a.merge(&hll_b);
        let merged_count = hll_a.count();

        // Merged count should be roughly count_a + count_b (no overlap)
        assert!(merged_count > count_a);
        assert!(merged_count > count_b);
        let expected = 1000;
        let lower_bound = (expected as f64 * 0.75) as u64;
        let upper_bound = (expected as f64 * 1.25) as u64;
        assert!(
            merged_count >= lower_bound && merged_count <= upper_bound,
            "Expected merged count near {}, got {} (bounds: {}-{})", expected, merged_count, lower_bound, upper_bound
        );
    }

    #[test]
    fn test_hyperloglog_merge_with_overlap() {
        let mut hll_a = HyperLogLog::new();
        let mut hll_b = HyperLogLog::new();

        // Both see the same 500 items
        for i in 0..500 {
            hll_a.insert(format!("shared_{}", i).as_bytes());
            hll_b.insert(format!("shared_{}", i).as_bytes());
        }

        hll_a.merge(&hll_b);
        let merged_count = hll_a.count();

        // Should still be ~500, not ~1000
        let lower_bound = (500_f64 * 0.75) as u64;
        let upper_bound = (500_f64 * 1.25) as u64;
        assert!(
            merged_count >= lower_bound && merged_count <= upper_bound,
            "Expected merged count near 500, got {} (bounds: {}-{})", merged_count, lower_bound, upper_bound
        );
    }

    #[test]
    fn test_hyperloglog_small_counts() {
        for expected in [2, 5, 10, 20, 50] {
            let mut hll = HyperLogLog::new();
            for i in 0..expected {
                hll.insert(format!("item_{}", i).as_bytes());
            }
            let estimated_count = hll.count();
            // Wider tolerance for small counts
            let lower_bound = (expected as f64 * 0.5) as u64;
            let upper_bound = (expected as f64 * 2.0) as u64;
            assert!(
                estimated_count >= lower_bound && estimated_count <= upper_bound,
                "For {} items: expected count near {}, got {} (bounds: {}-{})", expected, expected, estimated_count, lower_bound, upper_bound
            );
        }
    }
}

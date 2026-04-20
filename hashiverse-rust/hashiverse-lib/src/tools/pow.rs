//! This crate provides a PoW mechanism that should be hugely expensive to reproduce on dedicated hardware.
//! Given that Hashiverse is predominantly built upon proof of work, we want to put in some effort to make it difficult to cheat as a spammer or sybil using GPU or ASIC advantages.
//! To this effect we cobble together a chain of different hashing algorithms with different repetition counts - all of which are pseudorandomly chosen based on the initial salt and each subsequent hashing round.

use crate::tools::pow_required_estimator::PowRequiredEstimator;
use crate::tools::time_provider::time_provider::{RealTimeProvider, TimeProvider};
use crate::tools::types::{Hash, Pow, Salt};
use crate::tools::{hashing, tools};
use digest::consts::{U32, U64};

use digest::generic_array::GenericArray;
use digest::Digest;
use log::trace;

fn apply_hash<H>(data: &Hash) -> anyhow::Result<Hash>
where H: Digest
{
    Hash::from_slice(&H::digest(data.as_ref()).as_slice()[0..32])
}

fn apply_chained_hash(algo_index: usize, hash_current: Hash) -> anyhow::Result<Hash> {

    const ALGO_COUNT: usize = 17;
    let algo_index = algo_index % ALGO_COUNT;

    match algo_index {
        // --- Cases 0-13: Use the existing clean apply_hash ---
        0 => apply_hash::<blake2::Blake2s256>(&hash_current),
        1 => apply_hash::<blake2::Blake2b512>(&hash_current),
        2 => apply_hash::<sha2::Sha256>(&hash_current),
        3 => apply_hash::<sha2::Sha384>(&hash_current),
        4 => apply_hash::<sha2::Sha512>(&hash_current),
        5 => apply_hash::<sha3::Sha3_256>(&hash_current),
        6 => apply_hash::<sha3::Sha3_384>(&hash_current),
        7 => apply_hash::<sha3::Sha3_512>(&hash_current),
        8 => apply_hash::<sha3::Keccak256>(&hash_current),
        9 => apply_hash::<sha3::Keccak384>(&hash_current),
        10 => apply_hash::<sha3::Keccak512>(&hash_current),
        11 => apply_hash::<groestl::Groestl256>(&hash_current),
        12 => apply_hash::<groestl::Groestl512>(&hash_current),
        13 => apply_hash::<whirlpool::Whirlpool>(&hash_current),

        // --- Cases 14-16: Keep custom logic here ---
        14 => {
            let mut hasher = skein::Skein256::new();
            hasher.update(hash_current.as_ref());
            let hash_output: GenericArray<u8, U32> = hasher.finalize();
            Hash::from_slice(&hash_output.as_slice()[0..32])
        },
        15 => {
            let mut hasher = skein::Skein512::new();
            hasher.update(hash_current.as_ref());
            let hash_output: GenericArray<u8, U64> = hasher.finalize();
            Hash::from_slice(&hash_output.as_slice()[0..32])
        },
        16 => {
            let mut hasher = blake3::Hasher::new();
            hasher.update(hash_current.as_ref());
            let hash_output = hasher.finalize();
            Hash::from_slice(&hash_output.as_bytes()[0..32])
        },

        _ => Ok(hash_current),
    }
}

/// Pre-hash all input data into a single 32-byte `Hash`.
///
/// Call this once before the iteration loop; pass the result to
/// `pow_measure_from_data_hash` / `pow_generate_with_iteration_limit` so that
/// workers only receive 32 bytes instead of the full raw data.
pub fn pow_compute_data_hash(datas: &[&[u8]]) -> Hash {
    hashing::hash_multiple(datas)
}

/// Core PoW measurement given an already-pre-hashed data blob.
///
/// Computes `hash(data_hash ++ salt)` as the starting point, then runs the
/// 5-round chained-hash algorithm.  Use `pow_compute_data_hash` to produce
/// `data_hash` from raw inputs.
pub fn pow_measure_from_data_hash(data_hash: &Hash, salt: &Salt) -> anyhow::Result<(Pow, Hash)> {
    let mut data_current = hashing::hash_two(data_hash.as_ref(), salt.as_ref());

    const CHAIN_LENGTH: usize = 5;
    const MAX_REPETITIONS: usize = 2;

    for _ in 0..CHAIN_LENGTH {
        let algo_index = data_current.as_bytes()[0] as usize;
        let repetitions = data_current.as_bytes()[1] as usize % MAX_REPETITIONS;

        for _ in 0..=repetitions {
            data_current = apply_chained_hash(algo_index, data_current)?;
        }
    }

    let leading_zero_bits = tools::count_leading_zero_bits(data_current.as_bytes());
    Ok((Pow(leading_zero_bits), data_current))
}

pub fn pow_measure(datas: &[&[u8]], salt: &Salt) -> anyhow::Result<(Pow, Hash)> {
    pow_measure_from_data_hash(&pow_compute_data_hash(datas), salt)
}

/// Try find a sufficient PoW.
///
/// It will return the best so far if is it unsuccessful after the `iteration_limit`
pub async fn pow_generate_with_iteration_limit(iteration_limit: usize, pow_min: Pow, data_hash: &Hash) -> anyhow::Result<(Salt, Pow, Hash)> {
    let mut salt: Salt;
    let mut pow_best_so_far = (Salt::zero(), Pow(0), Hash::zero());

    for _ in 0..iteration_limit {
        salt = Salt::random();
        let (pow, hash) = pow_measure_from_data_hash(data_hash, &salt)?;

        if pow >= pow_min {
            return Ok((salt, pow, hash));
        }

        if pow > pow_best_so_far.1 {
            pow_best_so_far = (salt, pow, hash);
        }
    }

    Ok(pow_best_so_far)
}

/// Try forever until a valid PoW is found.
///
/// This method "yields" occasionally so that other tokio processes can make progress.
pub async fn pow_generate(pow_required: Pow, datas: &[&[u8]]) -> anyhow::Result<(Salt, Pow, Hash)> {
    const BATCH_SIZE: usize = 64 * 1024;
    let real_time_provider = RealTimeProvider::default();
    let mut estimator = PowRequiredEstimator::new(real_time_provider.current_time_millis(), "pow_generate", pow_required);
    let data_hash = pow_compute_data_hash(datas);
    loop {
        let result = pow_generate_with_iteration_limit(BATCH_SIZE, pow_required, &data_hash).await?;
        if result.1 >= pow_required {
            return Ok(result);
        }

        let progress = estimator.record_batch_and_estimate(real_time_provider.current_time_millis(), BATCH_SIZE, result.1);
        trace!("{}", progress);
        tools::yield_now().await;
    }
}

#[cfg(test)]
mod tests {
    use crate::tools::pow::{pow_compute_data_hash, pow_generate, pow_measure, pow_measure_from_data_hash};
    use crate::tools::tools;
    use crate::tools::types::{Pow, Salt};

    #[tokio::test]
    async fn pow_test() {
        for _ in 1..1000 {
            let mut data1 = [0u8; 1024];
            tools::random_fill_bytes(&mut data1);
            let mut data2 = [0u8; 512];
            tools::random_fill_bytes(&mut data2);

            let salt = Salt::random();
            let _pow = pow_measure(&[&data1, &data2], &salt);
        }
    }

    #[tokio::test]
    async fn pow_generate_test() -> anyhow::Result<()> {
        const POW_MIN: Pow = Pow(16);

        let mut data = [0u8; 1024];
        tools::random_fill_bytes(&mut data);
        let (salt, _, _) = pow_generate(POW_MIN, &[&data]).await?;
        let (pow, _) = pow_measure(&[&data], &salt)?;
        assert!(pow >= POW_MIN);

        Ok(())
    }

    /// `pow_measure` must produce the same result as pre-hashing then calling
    /// `pow_measure_from_data_hash` — the two-step path used by parallel workers.
    #[tokio::test]
    async fn pow_measure_and_from_data_hash_agree() -> anyhow::Result<()> {
        for _ in 0..200 {
            let mut data1 = [0u8; 256];
            tools::random_fill_bytes(&mut data1);
            let mut data2 = [0u8; 128];
            tools::random_fill_bytes(&mut data2);
            let salt = Salt::random();

            let (pow_direct, hash_direct) = pow_measure(&[&data1, &data2], &salt)?;
            let data_hash = pow_compute_data_hash(&[&data1, &data2]);
            let (pow_split, hash_split) = pow_measure_from_data_hash(&data_hash, &salt)?;

            assert_eq!(pow_direct, pow_split);
            assert_eq!(hash_direct, hash_split);
        }
        Ok(())
    }
}

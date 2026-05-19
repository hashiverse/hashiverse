//! This crate provides a PoW mechanism that should be hugely expensive to reproduce on dedicated hardware.
//! Given that Hashiverse is predominantly built upon proof of work, we want to put in some effort to make it difficult to cheat as a spammer or sybil using GPU or ASIC advantages.
//! To this effect we cobble together a chain of different hashing algorithms with different repetition counts - all of which are pseudorandomly chosen based on the initial salt and each subsequent hashing round.

use crate::tools::pow_required_estimator::PowRequiredEstimator;
use crate::tools::time_provider::time_provider::{RealTimeProvider, TimeProvider};
use crate::tools::types::{Hash, Pow, Salt};
use crate::tools::{hashing, tools};
use digest::consts::{U32, U64};

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
        14 => apply_hash::<skein::Skein256<U32>>(&hash_current),
        15 => apply_hash::<skein::Skein512<U64>>(&hash_current),

        // Unfortunately the "digest" trait of the blake3 crate is an older version than we need...so hand roll.
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

    struct RegressionVector {
        label: &'static str,
        datas: Vec<Vec<u8>>,
        salt_hex: &'static str,
        expected_pow: u8,
        expected_final_hash_hex: &'static str,
    }

    fn regression_vectors() -> Vec<RegressionVector> {
        vec![
            RegressionVector {
                label: "empty_data_zero_salt",
                datas: vec![hex::decode("").unwrap()],
                salt_hex: "0000000000000000",
                expected_pow: 1,
                expected_final_hash_hex: "6eec432cb487409c9500776340420e6281ac4aa06c2b7ec39916828dbf8bb39e",
            },
            RegressionVector {
                label: "single_byte_zero_data",
                datas: vec![hex::decode("00").unwrap()],
                salt_hex: "0000000000000001",
                expected_pow: 4,
                expected_final_hash_hex: "0db42c05e85a32ac76f14c4a3132e8b82c0f97ba49e5bf0464173067ec20e86d",
            },
            RegressionVector {
                label: "single_byte_high",
                datas: vec![hex::decode("ff").unwrap()],
                salt_hex: "ffffffffffffffff",
                expected_pow: 0,
                expected_final_hash_hex: "8b3754ac665ff1cdff1539552511b23b320c9865f0989add3bcaa245798be5a8",
            },
            RegressionVector {
                label: "ascii_short",
                datas: vec![hex::decode("68617368697665727365").unwrap()],
                salt_hex: "0123456789abcdef",
                expected_pow: 0,
                expected_final_hash_hex: "adb2ef187879beb0615fc054ef2c94fcda7f022226b5f86d92485c00161bf081",
            },
            RegressionVector {
                label: "ascii_long",
                datas: vec![hex::decode("54686520717569636b2062726f776e20666f78206a756d7073206f76657220746865206c617a7920646f67").unwrap()],
                salt_hex: "deadbeefcafebabe",
                expected_pow: 0,
                expected_final_hash_hex: "9502fa8669c3a191e3c0f3a59ce5b630c97c0460262cc8235b225ec88f05b27c",
            },
            RegressionVector {
                label: "two_chunks_ascii",
                datas: vec![hex::decode("68656c6c6f").unwrap(), hex::decode("776f726c64").unwrap()],
                salt_hex: "fedcba9876543210",
                expected_pow: 0,
                expected_final_hash_hex: "8885e4616f1b5657c954a587a260c05d820c028fcc792ca27741d943507c397d",
            },
            RegressionVector {
                label: "three_chunks_mixed",
                datas: vec![hex::decode("61").unwrap(), hex::decode("6262").unwrap(), hex::decode("636363").unwrap()],
                salt_hex: "1111111111111111",
                expected_pow: 2,
                expected_final_hash_hex: "2ca9bb4af0b9946d7b4ec2591f80148235355c75d4a6bb808257980c2546f6a3",
            },
            RegressionVector {
                label: "binary_pattern",
                datas: vec![hex::decode("0001020304050607").unwrap()],
                salt_hex: "8000000000000000",
                expected_pow: 3,
                expected_final_hash_hex: "1080bab28b0963e88416512f313172774a0b1e538912928ed870f28c402d2e29",
            },
            RegressionVector {
                label: "256_byte_zeroes",
                datas: vec![hex::decode("00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000").unwrap()],
                salt_hex: "a5a5a5a5a5a5a5a5",
                expected_pow: 2,
                expected_final_hash_hex: "39c0c67e6d471fee89b36f484237af21a83e2ab5113ff0c7a2adc66ce95a7de9",
            },
            RegressionVector {
                label: "256_byte_ones",
                datas: vec![hex::decode("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff").unwrap()],
                salt_hex: "5a5a5a5a5a5a5a5a",
                expected_pow: 0,
                expected_final_hash_hex: "eb532e53c96611844a9d7cb3417740572f7455a54f5a9c77dec08d24eccb3b26",
            },
        ]
    }

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

    /// Regression guard: a future dep bump that silently changes the output of any of the 17
    /// chained hash algorithms (or of the blake3 pre-hash / hash-two step) will fail this test.
    ///
    /// Hard-coded vectors are captured against the current crate versions. When an *intentional*
    /// future bump changes output, regenerate via the `pow_regression_vectors_print` helper.
    #[test]
    fn pow_regression_vectors_match() -> anyhow::Result<()> {
        for vector in regression_vectors() {
            let salt_bytes = hex::decode(vector.salt_hex).expect("salt_hex must be valid hex");
            let salt = Salt::from_slice(&salt_bytes)?;
            let data_refs: Vec<&[u8]> = vector.datas.iter().map(|v| v.as_slice()).collect();

            let (pow_direct, hash_direct) = pow_measure(&data_refs, &salt)?;
            let data_hash = pow_compute_data_hash(&data_refs);
            let (pow_split, hash_split) = pow_measure_from_data_hash(&data_hash, &salt)?;

            assert_eq!(pow_direct, pow_split, "vector {}: two-path mismatch", vector.label);
            assert_eq!(hash_direct, hash_split, "vector {}: two-path mismatch", vector.label);
            assert_eq!(pow_direct, Pow(vector.expected_pow), "vector {}: pow drift (likely crypto crate output change)", vector.label);
            assert_eq!(hex::encode(hash_direct.as_bytes()), vector.expected_final_hash_hex, "vector {}: final-hash drift (likely crypto crate output change)", vector.label);
        }
        Ok(())
    }


    /*
    /// Run with: `cargo nextest run -p hashiverse-lib pow::tests::pow_regression_vectors_print --run-ignored ignored-only --no-capture`
    /// then paste the printed RegressionVector blocks back into `regression_vectors()`.
    #[test]
    #[ignore]
    fn pow_regression_vectors_print() -> anyhow::Result<()> {
        println!();
        println!("// --- begin regenerated PoW regression vectors ---");
        for vector in regression_vectors() {
            let salt_bytes = hex::decode(vector.salt_hex).expect("salt_hex must be valid hex");
            let salt = Salt::from_slice(&salt_bytes)?;
            let data_refs: Vec<&[u8]> = vector.datas.iter().map(|v| v.as_slice()).collect();

            let (pow, hash) = pow_measure(&data_refs, &salt)?;

            let datas_literal = vector
                .datas
                .iter()
                .map(|d| format!("hex::decode(\"{}\").unwrap()", hex::encode(d)))
                .collect::<Vec<_>>()
                .join(", ");

            println!("RegressionVector {{");
            println!("    label: \"{}\",", vector.label);
            println!("    datas: vec![{}],", datas_literal);
            println!("    salt_hex: \"{}\",", vector.salt_hex);
            println!("    expected_pow: {},", pow.0);
            println!("    expected_final_hash_hex: \"{}\",", hex::encode(hash.as_bytes()));
            println!("}},");
        }
        println!("// --- end regenerated PoW regression vectors ---");
        Ok(())
    }
    */
}

//! Sparse Distributed Representation (SDR) encoding and operations.
//!
//! An SDR is a fixed-size binary vector where each bit indicates the presence
//! of a learned feature. SDRs enable sub-millisecond associative recall via
//! Hamming distance (XOR + popcount).

/// Number of bits in the SDR.
pub const SDR_BITS: usize = 256;

/// Number of u64 words in a packed SDR.
pub const SDR_WORDS: usize = SDR_BITS / 64; // 4

/// A packed SDR: 256 bits stored as 4 × u64 words (32 bytes total).
pub type Sdr = [u64; SDR_WORDS];

/// An empty SDR (all bits zero).
pub const ZERO: Sdr = [0u64; SDR_WORDS];

/// Binarize a dense float embedding into an SDR via sign-thresholding.
///
/// Takes the first `SDR_BITS` dimensions of the embedding.
/// Bit `i` is set to 1 if `embedding[i] >= 0.0`, else 0.
///
/// # Panics
/// Panics if `embedding.len() < SDR_BITS`.
pub fn binarize(embedding: &[f32]) -> Sdr {
    assert!(
        embedding.len() >= SDR_BITS,
        "embedding must have at least {SDR_BITS} dimensions, got {}",
        embedding.len()
    );
    let mut sdr = ZERO;
    for i in 0..SDR_BITS {
        if embedding[i] >= 0.0 {
            sdr[i / 64] |= 1u64 << (i % 64);
        }
    }
    sdr
}

/// Hamming distance between two SDRs (number of differing bits).
pub fn hamming_distance(a: &Sdr, b: &Sdr) -> u32 {
    let mut dist = 0u32;
    for i in 0..SDR_WORDS {
        dist += (a[i] ^ b[i]).count_ones();
    }
    dist
}

/// Normalized Hamming similarity: `1.0 - (distance / SDR_BITS)`.
///
/// Returns a value in `[0.0, 1.0]` where 1.0 means identical SDRs.
pub fn hamming_similarity(a: &Sdr, b: &Sdr) -> f64 {
    1.0 - (hamming_distance(a, b) as f64 / SDR_BITS as f64)
}

/// Serialize an SDR to bytes (little-endian) for PostgreSQL BYTEA storage.
pub fn to_bytes(sdr: &Sdr) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(SDR_WORDS * 8);
    for word in sdr {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes
}

/// Deserialize an SDR from bytes (little-endian).
///
/// # Panics
/// Panics if `bytes.len() < SDR_WORDS * 8`.
pub fn from_bytes(bytes: &[u8]) -> Sdr {
    assert!(
        bytes.len() >= SDR_WORDS * 8,
        "need at least {} bytes, got {}",
        SDR_WORDS * 8,
        bytes.len()
    );
    let mut sdr = ZERO;
    for (i, chunk) in bytes.chunks_exact(8).take(SDR_WORDS).enumerate() {
        sdr[i] = u64::from_le_bytes(chunk.try_into().unwrap());
    }
    sdr
}

/// Convert an SDR to a Vec<f32> of 0.0/1.0 values for Qdrant upsert.
///
/// Qdrant does not natively store binary vectors, so we represent each bit
/// as a float. With BinaryQuantization enabled, Qdrant compresses these
/// back to 1 bit per dimension internally.
pub fn to_f32_vec(sdr: &Sdr) -> Vec<f32> {
    let mut vec = Vec::with_capacity(SDR_BITS);
    for i in 0..SDR_BITS {
        let word = sdr[i / 64];
        let bit = (word >> (i % 64)) & 1;
        vec.push(bit as f32);
    }
    vec
}

/// Count the number of set bits (population count) in the SDR.
pub fn popcount(sdr: &Sdr) -> u32 {
    sdr.iter().map(|w| w.count_ones()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binarize_sign_threshold() {
        let mut embedding = vec![0.0f32; SDR_BITS];
        // Set first 8 dims positive, rest negative
        for i in 0..8 {
            embedding[i] = 1.0;
        }
        for i in 8..SDR_BITS {
            embedding[i] = -1.0;
        }
        // 0.0 is treated as positive (>= 0.0)
        let sdr = binarize(&embedding);
        // First 8 bits should be set in word 0
        assert_eq!(sdr[0] & 0xFF, 0xFF);
    }

    #[test]
    fn hamming_distance_identical() {
        let a: Sdr = [0xDEAD_BEEF, 0xCAFE_1234, 0x1111_2222, 0x3333_4444];
        assert_eq!(hamming_distance(&a, &a), 0);
        assert_eq!(hamming_similarity(&a, &a), 1.0);
    }

    #[test]
    fn hamming_distance_all_different() {
        let a: Sdr = [u64::MAX; SDR_WORDS];
        let b = ZERO;
        assert_eq!(hamming_distance(&a, &b), SDR_BITS as u32);
        assert_eq!(hamming_similarity(&a, &b), 0.0);
    }

    #[test]
    fn byte_round_trip() {
        let sdr: Sdr = [
            0xDEAD_BEEF_CAFE_1234,
            0x5678_9ABC_DEF0_1234,
            0xAAAA_BBBB_CCCC_DDDD,
            0x1111_2222_3333_4444,
        ];
        let bytes = to_bytes(&sdr);
        assert_eq!(bytes.len(), 32);
        let restored = from_bytes(&bytes);
        assert_eq!(sdr, restored);
    }

    #[test]
    fn to_f32_vec_length() {
        let sdr: Sdr = [1, 0, 0, 0]; // only bit 0 set
        let vec = to_f32_vec(&sdr);
        assert_eq!(vec.len(), SDR_BITS);
        assert_eq!(vec[0], 1.0);
        assert_eq!(vec[1], 0.0);
    }

    #[test]
    fn binarize_deterministic() {
        let embedding: Vec<f32> = (0..SDR_BITS).map(|i| (i as f32) - 128.0).collect();
        let sdr1 = binarize(&embedding);
        let sdr2 = binarize(&embedding);
        assert_eq!(sdr1, sdr2);
    }

    #[test]
    fn popcount_works() {
        let sdr: Sdr = [0xFF, 0, 0, 0]; // 8 bits set
        assert_eq!(popcount(&sdr), 8);
    }
}

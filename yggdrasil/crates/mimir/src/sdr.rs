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

/// Convert an SDR to a bipolar Vec<f32> of {-1.0, 1.0} values for Qdrant upsert.
///
/// Maps {0,1} bits to {-1.0, 1.0} (bipolar encoding) so that Qdrant Dot product
/// distance is strictly rank-equivalent to Hamming distance:
///
///   A' · B' = 256 - 2·H(A,B)
///
/// where A', B' ∈ {-1, 1}^256.
///
/// Using {0.0, 1.0} is WRONG for Dot product — it measures intersection
/// (co-occurring 1s) rather than Hamming distance, making vectors with
/// different popcount incomparable.
pub fn to_bipolar_f32(sdr: &Sdr) -> Vec<f32> {
    let mut vec = Vec::with_capacity(SDR_BITS);
    for i in 0..SDR_BITS {
        let word = sdr[i / 64];
        let bit = (word >> (i % 64)) & 1;
        vec.push(if bit == 1 { 1.0 } else { -1.0 });
    }
    vec
}

/// Count the number of set bits (population count) in the SDR.
pub fn popcount(sdr: &Sdr) -> u32 {
    sdr.iter().map(|w| w.count_ones()).sum()
}

/// Bitwise AND of two SDRs — concept intersection.
///
/// The result contains only features present in both inputs.
pub fn and(a: &Sdr, b: &Sdr) -> Sdr {
    let mut out = ZERO;
    for i in 0..SDR_WORDS {
        out[i] = a[i] & b[i];
    }
    out
}

/// Bitwise OR of two SDRs — concept union.
///
/// The result contains features present in either input.
pub fn or(a: &Sdr, b: &Sdr) -> Sdr {
    let mut out = ZERO;
    for i in 0..SDR_WORDS {
        out[i] = a[i] | b[i];
    }
    out
}

/// Bitwise XOR of two SDRs — symmetric difference.
///
/// The result contains features unique to one input but not the other.
pub fn xor(a: &Sdr, b: &Sdr) -> Sdr {
    let mut out = ZERO;
    for i in 0..SDR_WORDS {
        out[i] = a[i] ^ b[i];
    }
    out
}

/// Jaccard similarity between two SDRs: `popcount(AND) / popcount(OR)`.
///
/// Returns 0.0 if both SDRs are all-zero (no features).
/// Returns a value in `[0.0, 1.0]` where 1.0 means identical feature sets.
pub fn jaccard(a: &Sdr, b: &Sdr) -> f64 {
    let intersection = popcount(&and(a, b)) as f64;
    let union = popcount(&or(a, b)) as f64;
    if union == 0.0 {
        0.0
    } else {
        intersection / union
    }
}

/// Dot-product similarity between two L2-normalized embeddings.
///
/// Since the ONNX embedder (all-MiniLM-L6-v2) produces L2-normalized output,
/// dot product equals cosine similarity. Returns a value in `[-1.0, 1.0]`
/// where 1.0 means identical, 0.0 means unrelated, -1.0 means opposite.
pub fn dot_similarity(a: &[f32], b: &[f32]) -> f64 {
    debug_assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| (*x as f64) * (*y as f64))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_similarity_identical() {
        let a = vec![0.5, 0.5, 0.5, 0.5];
        assert!((dot_similarity(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn dot_similarity_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!(dot_similarity(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn dot_similarity_opposite() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        assert!((dot_similarity(&a, &b) + 1.0).abs() < 1e-6);
    }

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
    fn to_bipolar_f32_length_and_values() {
        let sdr: Sdr = [1, 0, 0, 0]; // only bit 0 set
        let vec = to_bipolar_f32(&sdr);
        assert_eq!(vec.len(), SDR_BITS);
        // bit 0 is set → 1.0
        assert_eq!(vec[0], 1.0);
        // bit 1 is unset → -1.0 (bipolar, NOT 0.0)
        assert_eq!(vec[1], -1.0);
    }

    #[test]
    fn bipolar_dot_product_equals_hamming() {
        let a: Sdr = [0xFF, 0xF0, 0, 0]; // 12 bits set
        let b: Sdr = [0x0F, 0xFF, 0, 0]; // 12 bits set
        let va = to_bipolar_f32(&a);
        let vb = to_bipolar_f32(&b);
        let dot: f32 = va.iter().zip(vb.iter()).map(|(x, y)| x * y).sum();
        let hamming = hamming_distance(&a, &b);
        // A'·B' = 256 - 2·H(A,B)
        assert_eq!(dot as i32, SDR_BITS as i32 - 2 * hamming as i32);
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

    #[test]
    fn and_intersection() {
        let a: Sdr = [0xFF, 0xF0, 0, 0];
        let b: Sdr = [0x0F, 0xFF, 0, 0];
        let result = and(&a, &b);
        assert_eq!(result, [0x0F, 0xF0, 0, 0]);
    }

    #[test]
    fn or_union() {
        let a: Sdr = [0xFF, 0x00, 0, 0];
        let b: Sdr = [0x00, 0xFF, 0, 0];
        let result = or(&a, &b);
        assert_eq!(result, [0xFF, 0xFF, 0, 0]);
    }

    #[test]
    fn xor_symmetric_difference() {
        let a: Sdr = [0xFF, 0xF0, 0, 0];
        let b: Sdr = [0xFF, 0x0F, 0, 0];
        let result = xor(&a, &b);
        assert_eq!(result, [0x00, 0xFF, 0, 0]);
    }

    #[test]
    fn jaccard_identical() {
        let a: Sdr = [0xFF, 0xFF, 0xFF, 0xFF];
        assert_eq!(jaccard(&a, &a), 1.0);
    }

    #[test]
    fn jaccard_disjoint() {
        let a: Sdr = [0xFF, 0, 0, 0];
        let b: Sdr = [0, 0xFF, 0, 0];
        // AND = 0 bits, OR = 16 bits → Jaccard = 0.0
        assert_eq!(jaccard(&a, &b), 0.0);
    }

    #[test]
    fn jaccard_zero_sdrs() {
        assert_eq!(jaccard(&ZERO, &ZERO), 0.0);
    }

    #[test]
    fn jaccard_partial_overlap() {
        let a: Sdr = [0xFF, 0xFF, 0, 0]; // 16 bits
        let b: Sdr = [0xFF, 0, 0, 0];    // 8 bits
        // AND = 8 bits, OR = 16 bits → Jaccard = 0.5
        assert_eq!(jaccard(&a, &b), 0.5);
    }
}

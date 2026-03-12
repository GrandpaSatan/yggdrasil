//! Core SDR (Sparse Distributed Representation) type and operations.
//!
//! Shared across crates (mimir, odin) for consistent SDR handling.
//! An SDR is a fixed-size 256-bit binary vector stored as 4 × u64 words.

/// Number of bits in the SDR.
pub const SDR_BITS: usize = 256;

/// Number of u64 words in a packed SDR.
pub const SDR_WORDS: usize = SDR_BITS / 64; // 4

/// A packed SDR: 256 bits stored as 4 × u64 words (32 bytes total).
pub type Sdr = [u64; SDR_WORDS];

/// An empty SDR (all bits zero).
pub const ZERO: Sdr = [0u64; SDR_WORDS];

/// Hamming distance between two SDRs (number of differing bits).
pub fn hamming_distance(a: &Sdr, b: &Sdr) -> u32 {
    let mut dist = 0u32;
    for i in 0..SDR_WORDS {
        dist += (a[i] ^ b[i]).count_ones();
    }
    dist
}

/// Normalized Hamming similarity: `1.0 - (distance / SDR_BITS)`.
pub fn hamming_similarity(a: &Sdr, b: &Sdr) -> f64 {
    1.0 - (hamming_distance(a, b) as f64 / SDR_BITS as f64)
}

/// Bitwise OR of two SDRs — concept union.
pub fn or(a: &Sdr, b: &Sdr) -> Sdr {
    let mut out = ZERO;
    for i in 0..SDR_WORDS {
        out[i] = a[i] | b[i];
    }
    out
}

/// Count the number of set bits (population count) in the SDR.
pub fn popcount(sdr: &Sdr) -> u32 {
    sdr.iter().map(|w| w.count_ones()).sum()
}

/// Deserialize an SDR from a little-endian byte slice.
///
/// Returns `None` if the slice is shorter than 32 bytes.
pub fn from_bytes(bytes: &[u8]) -> Option<Sdr> {
    if bytes.len() < SDR_WORDS * 8 {
        return None;
    }
    let mut sdr = ZERO;
    for (i, chunk) in bytes.chunks_exact(8).take(SDR_WORDS).enumerate() {
        sdr[i] = u64::from_le_bytes(chunk.try_into().unwrap());
    }
    Some(sdr)
}

/// Parse an SDR from a hex string (64 hex chars = 32 bytes).
///
/// Returns `None` if the hex is invalid or too short.
pub fn from_hex(hex: &str) -> Option<Sdr> {
    if hex.len() < SDR_WORDS * 16 {
        return None;
    }
    let mut bytes = Vec::with_capacity(SDR_WORDS * 8);
    for i in (0..SDR_WORDS * 16).step_by(2) {
        let byte = u8::from_str_radix(&hex[i..i + 2], 16).ok()?;
        bytes.push(byte);
    }
    from_bytes(&bytes)
}

/// Encode an SDR as a hex string (64 hex chars = 32 bytes, little-endian).
pub fn to_hex(sdr: &Sdr) -> String {
    let mut out = String::with_capacity(SDR_WORDS * 16);
    for word in sdr {
        for byte in word.to_le_bytes() {
            out.push_str(&format!("{byte:02x}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_bytes_round_trip() {
        let sdr: Sdr = [0xDEAD_BEEF_CAFE_1234, 0x5678_9ABC_DEF0_1234, 0xAAAA_BBBB, 0x1111_2222];
        // Serialize to LE bytes
        let mut bytes = Vec::new();
        for w in &sdr {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        let restored = from_bytes(&bytes).expect("valid 32 bytes");
        assert_eq!(sdr, restored);
    }

    #[test]
    fn from_bytes_too_short_returns_none() {
        assert!(from_bytes(&[0u8; 31]).is_none());
        assert!(from_bytes(&[]).is_none());
    }

    #[test]
    fn from_bytes_exact_32_bytes_works() {
        assert!(from_bytes(&[0u8; 32]).is_some());
    }

    #[test]
    fn hex_round_trip() {
        let sdr: Sdr = [0xFF00_FF00_FF00_FF00, 0x1234_5678_9ABC_DEF0, 0, u64::MAX];
        let hex = to_hex(&sdr);
        assert_eq!(hex.len(), 64);
        let restored = from_hex(&hex).expect("valid hex");
        assert_eq!(sdr, restored);
    }

    #[test]
    fn from_hex_too_short_returns_none() {
        // 63 chars is not enough (need 64)
        assert!(from_hex(&"a".repeat(63)).is_none());
        assert!(from_hex("").is_none());
    }

    #[test]
    fn from_hex_invalid_chars_returns_none() {
        // 'zz' is not valid hex
        let bad = "zz".to_string() + &"00".repeat(31);
        assert!(from_hex(&bad).is_none());
    }

    #[test]
    fn from_hex_all_zeros() {
        let hex = "00".repeat(32);
        let sdr = from_hex(&hex).expect("valid hex");
        assert_eq!(sdr, ZERO);
    }

    #[test]
    fn from_hex_known_value() {
        // 0xFF little-endian in word 0 = byte 0 is 0xFF, rest 0x00
        let mut hex = String::from("ff");
        hex.push_str(&"00".repeat(31));
        let sdr = from_hex(&hex).expect("valid hex");
        assert_eq!(sdr[0], 0xFF); // byte 0 = 0xFF in LE u64
        assert_eq!(sdr[1], 0);
        assert_eq!(sdr[2], 0);
        assert_eq!(sdr[3], 0);
    }

    #[test]
    fn hamming_similarity_known_values() {
        // 50% overlap: words 0,1 all set in a, words 2,3 all set in b
        let a: Sdr = [u64::MAX, u64::MAX, 0, 0];
        let b: Sdr = [0, 0, u64::MAX, u64::MAX];
        // All 256 bits differ → similarity = 0.0
        assert_eq!(hamming_similarity(&a, &b), 0.0);

        // Half-overlap: word 0 shared, word 1 differs
        let c: Sdr = [u64::MAX, u64::MAX, 0, 0]; // 128 bits set
        let d: Sdr = [u64::MAX, 0, 0, 0];          // 64 bits set
        // Distance = 64 (word 1 differs), similarity = 1 - 64/256 = 0.75
        assert_eq!(hamming_similarity(&c, &d), 0.75);
    }

    #[test]
    fn or_is_superset() {
        let a: Sdr = [0xFF, 0, 0, 0];
        let b: Sdr = [0, 0xFF, 0, 0];
        let combined = or(&a, &b);
        // OR should contain all bits from both
        assert_eq!(popcount(&combined), popcount(&a) + popcount(&b));
    }

    #[test]
    fn popcount_full_and_empty() {
        assert_eq!(popcount(&ZERO), 0);
        assert_eq!(popcount(&[u64::MAX; SDR_WORDS]), SDR_BITS as u32);
    }
}

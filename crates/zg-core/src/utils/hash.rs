//! SHA-256 hex digests.

use sha2::{Digest, Sha256};

/// Hex sha256 of a text value.
#[must_use]
pub fn sha256_text(text: &str) -> String {
    sha256_bytes(text.as_bytes())
}

/// Lowercase hex of bytes, via a nibble lookup table (no per-byte `format!`).
/// Single spelling for grant signatures, fingerprints, and digests (R3).
#[must_use]
pub fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // Nibbles are always < 16; `.get()` only satisfies `indexing_slicing`.
        hex.push(HEX.get(usize::from(byte >> 4)).copied().unwrap_or(b'0') as char);
        hex.push(HEX.get(usize::from(byte & 0x0f)).copied().unwrap_or(b'0') as char);
    }
    hex
}

/// Hex sha256 of raw bytes.
#[must_use]
pub fn sha256_bytes(bytes: &[u8]) -> String {
    to_hex(Sha256::digest(bytes).as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_digest() {
        assert_eq!(
            sha256_text(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_text("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}

//! Strongly-typed identifiers used across the engine.
//!
//! Both ids are opaque hex strings on the wire; the newtypes prevent accidental
//! substitution of one for the other and centralize validation.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{EngineError, EngineResult, codes};

/// Identifier of an indexed file (32 hex chars, derived from path).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FileId(String);

impl FileId {
    /// Wraps an existing id string without validation (trusted internal data).
    pub fn from_raw(raw: String) -> Self {
        Self(raw)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identifier of an indexed entity fragment (64-char sha256 hex of
/// `{file_id}\0{index}`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EntityId(String);

impl EntityId {
    /// Wraps an existing id string without validation (trusted internal data).
    pub fn from_raw(raw: String) -> Self {
        Self(raw)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parses a 64-char lowercase hex entity id.
    pub fn parse(value: &str) -> EngineResult<Self> {
        if value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            Ok(Self(value.to_owned()))
        } else {
            Err(
                EngineError::new(codes::manifest_invalid(), "invalid entity id")
                    .with_context(format!("entityId={value}")),
            )
        }
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Computes an entity id from its file id and fragment index:
/// sha256(`${file_id}\0${index}`), hex-encoded.
pub fn make_entity_id(file_id: &FileId, index: usize) -> EntityId {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(file_id.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(index.to_string().as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        hex.push_str(hex_char(byte >> 4));
        hex.push_str(hex_char(byte & 0x0f));
    }
    EntityId(hex)
}

fn hex_char(nibble: u8) -> &'static str {
    const HEX: [&str; 16] = [
        "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "a", "b", "c", "d", "e", "f",
    ];
    HEX[nibble as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_ids_are_deterministic_sha256_hex() {
        let file = FileId::from_raw("f".repeat(32));
        let a = make_entity_id(&file, 0);
        let b = make_entity_id(&file, 0);
        let c = make_entity_id(&file, 1);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.as_str().len(), 64);
        assert!(a.as_str().bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn parse_rejects_malformed_ids() {
        assert!(EntityId::parse(&"a".repeat(64)).is_ok());
        assert!(EntityId::parse(&"A".repeat(64)).is_err());
        assert!(EntityId::parse("short").is_err());
    }
}

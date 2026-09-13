//! Opaque job handle: a UUID string on the wire.
//!
//! Newtype over `String` (type-driven): callers can compare, hash, and
//! display ids but can never fabricate the inner string except through the
//! crate-internal constructor, so "valid job id" stays a compiler-checked
//! property of origin rather than a runtime convention.

/// Opaque job handle, a UUID string on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize)]
#[repr(transparent)]
pub struct JobId(String);

impl JobId {
    /// Crate-internal constructor; the only way to mint an id outside this
    /// module. Keeps the field private so external code cannot forge one.
    pub(crate) fn new(id: String) -> Self {
        Self(id)
    }

    /// Raw id string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

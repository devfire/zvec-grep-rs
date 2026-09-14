//! Hybrid `context()` search: request normalization ([`normalize`]),
//! per-group plan execution ([`search`]), and RRF merging ([`rank`]).

pub(super) mod normalize;
mod rank;
pub(super) mod search;

pub use rank::{DEFAULT_CONTEXT_LIMIT, DEFAULT_CONTEXT_TOTAL_LIMIT};

//! Tree-sitter code extractor: AST walk, entity fragmenting, outlines.
//!
//! Facade re-exporting the indexing entry point; the implementation lives in
//! the child modules (see [`entry`]).

pub mod chunking;
pub mod entry;
pub mod outline;
pub mod script_blocks;
pub mod walk;

pub use entry::extract_for_indexing;

//! Exhaustive lexical path: grep-crate powered text/regex search.
//!
//! In-process port of `engine/service/lexical.ts`. The TypeScript
//! implementation shells out to a ripgrep binary (`bundled-rg` with an `rg`
//! fallback); this port runs the same semantics inside the process with the
//! declared grep crates (`grep-regex` + `grep-searcher`) for matching and the
//! `ignore` crate for file discovery (hidden/ignore/max-depth/follow).
//!
//! Preserved TS semantics:
//! - multiple `--regexp` patterns (OR) plus `--file` pattern files,
//! - include/exclude path globs with `**/` expansion, `--glob`/`--iglob`
//!   filters, ripgrep file-type selection, hard-ignored `.git`/`.zvec-grep`,
//! - hidden/no-ignore/ignore-file/max-depth/max-filesize/follow options,
//! - `modifiedAfter`/`modifiedBefore` mtime post-filtering,
//! - `limit` + truncation reporting, missing-path diagnostics,
//! - before/after context expansion with `excerptRange`.
//!
//! Layout: `options` holds [`LexicalSearchOptions`] and diagnostics, `search`
//! holds [`run_lexical_search`], with `sink`, `context`, `patterns`,
//! `filter`, and `search_paths` behind them. This module only re-exports the
//! public surface so external callers are unaffected.

mod context;
pub mod enrichment;
mod filter;
mod options;
mod patterns;
mod search;
mod search_paths;
mod sink;

pub use enrichment::{
    STRUCTURE_ENRICH_FILE_LIMIT, StructureEnrichmentDiagnostics, StructureEnrichmentResult,
};

pub use filter::HARD_IGNORED_DIRECTORIES;
pub use options::{LexicalBackend, LexicalDiagnostics, LexicalSearchOptions, LexicalSearchResult};
pub use search::run_lexical_search;

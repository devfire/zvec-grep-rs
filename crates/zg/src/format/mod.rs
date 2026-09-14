//! Output renderers: hits, ranges, progress, and status (`cli/format/`).
//!
//! Every renderer is a pure `String`-builder covered by goldens; the thin
//! `print_*` wrappers own stdout/stderr. Layouts are idiomatic ports, not
//! byte copies, of `format/context.ts` + `format/status.ts` +
//! `format/progress.ts` + `format/range.ts` (see `docs/ts-divergence.md`):
//! the agent markdown keeps file:range headers, scores, and previews; the
//! human view keeps labeled fields with optional ANSI color.
//!
//! Layout: pure builders and their `print_*` wrappers live beside each
//! other per domain (`context_*`, `workspace`, `index`, `progress`,
//! `error`, `control`); shared paint lives in [`color`] and the
//! private `fields` helper, shared text shaping in [`text`] and
//! [`range`]. The facade re-exports the command surface the binary
//! uses, so existing call sites are untouched; pure builders stay in their
//! submodules and are imported from there (notably by the goldens).

mod color;
mod context;
mod context_agent;
mod context_human;
mod context_shared;
mod control;
mod error;
mod fields;
mod index;
mod progress;
mod range;
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
mod text;
mod workspace;

pub use color::{Color, use_color};
pub use context::{Rendering, print_context_result, print_context_warnings};
pub use control::print_control_status;
pub use error::print_error;
pub use index::{print_index_result, print_no_indexable_files_tip};
pub use progress::ProgressReporter;
pub use workspace::print_workspace_info;

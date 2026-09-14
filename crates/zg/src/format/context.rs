//! Context result I/O: layout dispatch plus stderr warnings.
//!
//! The pure builders live in [`context_agent`](super::context_agent) and
//! [`context_human`](super::context_human); this module owns only the thin
//! stdout/stderr wrappers.

use zg_core::service::types::ZvecGrepContextResult;

use super::color::Color;
use super::context_agent::format_context_agent;
use super::context_human::format_context_human;
use super::context_shared::empty_detail_lines;

/// Output layout for [`print_context_result`]: `--human` selects `Human`.
/// A distinct type (not `bool`) so the layout flag cannot be transposed
/// with [`Color`] at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rendering {
    /// Labeled fields for terminals.
    Human,
    /// Agent-consumed layout.
    Agent,
}

impl From<bool> for Rendering {
    /// `true` (i.e. `--human`) maps to [`Rendering::Human`].
    fn from(human: bool) -> Self {
        if human { Self::Human } else { Self::Agent }
    }
}

/// Prints the CLI query layout: [`Rendering::Human`] for `--human`,
/// [`Rendering::Agent`] otherwise.
pub fn print_context_result(result: &ZvecGrepContextResult, rendering: Rendering, color: Color) {
    if rendering == Rendering::Human {
        println!("{}", format_context_human(result, color.enabled()));
    } else {
        println!("{}", format_context_agent(result));
    }
}

/// Prints context warnings (empty reasons) to stderr.
pub fn print_context_warnings(result: &ZvecGrepContextResult) {
    for line in empty_detail_lines(result) {
        eprintln!("{line}");
    }
}

//! Labeled `label: value` fields with optional ANSI color.
//!
//! Single paint helper shared by the human context view and the workspace
//! status view, so both layouts stay visually identical by construction.

use super::color::{CYAN, RESET};

/// Renders one labeled field, colorizing the label when enabled.
#[must_use]
pub(crate) fn human_field(label: &str, value: &str, color: bool) -> String {
    if color {
        format!("{CYAN}{label}:{RESET} {value}")
    } else {
        format!("{label}: {value}")
    }
}

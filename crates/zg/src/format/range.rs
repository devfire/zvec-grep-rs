//! Range labels (`format/range.ts`).
//!
//! Every [`Range`] variant and every skipped field
//! is spelled out explicitly, so a new variant or field fails compilation
//! here instead of falling into a catch-all.

use zg_core::types::Range;

/// Renders a [`Range`] as `start-end`, `bytes:a-b`, `page:N`, or `file`,
/// mirroring `rangeLabel`.
// The `field: _` ignores below are intentional, not `..`: a new `Range` field
// must fail compilation here (see module docs). Allowed against
// `unneeded_field_pattern` for that exhaustiveness.
#[allow(clippy::unneeded_field_pattern)]
#[must_use]
pub fn range_label(range: &Range) -> String {
    match range {
        Range::Text {
            start_line,
            end_line,
            start_offset: _,
            end_offset: _,
        } => {
            if start_line == end_line {
                start_line.to_string()
            } else {
                format!("{start_line}-{end_line}")
            }
        }
        Range::Byte {
            start_offset,
            end_offset,
        } => format!("bytes:{start_offset}-{end_offset}"),
        Range::Page { page } => format!("page:{page}"),
        Range::PageText {
            page,
            start_offset: _,
            end_offset: _,
        } => format!("page:{page}"),
        Range::PageRegion {
            page,
            x: _,
            y: _,
            width: _,
            height: _,
        } => format!("page:{page}"),
        Range::File => "file".to_owned(),
    }
}

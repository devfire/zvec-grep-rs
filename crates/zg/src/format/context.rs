//! Context result I/O: layout dispatch plus stderr warnings.
//!
//! The pure builders live in [`context_agent`](super::context_agent) and
//! [`context_human`](super::context_human); this module owns only the thin
//! stdout/stderr wrappers.

use zg_core::service::types::ZvecGrepContextResult;

use super::context_agent::format_context_agent;
use super::context_human::format_context_human;
use super::context_shared::empty_detail_lines;

/// Prints the CLI query layout: human when `--human`, agent otherwise.
pub fn print_context_result(result: &ZvecGrepContextResult, human: bool, color: bool) {
    if human {
        println!("{}", format_context_human(result, color));
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

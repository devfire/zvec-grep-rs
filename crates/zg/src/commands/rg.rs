//! Direct `--rg`: in-process lexical search from CLI flags.
//!
//! Direct-only like the TypeScript CLI (`runDirectRgQuery` ignores the
//! transport mode), so no `rg` command string is ever built here.
//! Managed `rg` over the daemon is the `zvec_grep_rg` MCP tool.

use std::path::{Path, PathBuf};

use zg_core::lexical::LexicalSearchOptions;
use zg_core::service::facade::ZvecGrepService;
use zg_core::service::types::{ContextDiagnostics, ContextSource, ZvecGrepContextResult};

use super::support::service_options;
use crate::cli::{QueryArgs, parse_byte_size, parse_modified_time};
use crate::error::CliError;
use crate::format::{Rendering, print_context_result, use_color};

pub(crate) async fn run_rg_direct(args: QueryArgs, queries: Vec<String>) -> Result<(), CliError> {
    let color = use_color(args.color, args.no_color);
    let mut patterns = queries;
    patterns.extend(args.regexp.clone());
    if args.line_regexp {
        patterns = patterns
            .iter()
            .map(|pattern| format!("^(?:{pattern})$"))
            .collect();
    }
    let (before, after) = match (args.context, args.before_context, args.after_context) {
        (Some(both), None, None) => (both as usize, both as usize),
        _ => (
            args.before_context.map_or(0, |value| value as usize),
            args.after_context.map_or(0, |value| value as usize),
        ),
    };
    if args.context.is_some() && (args.before_context.is_some() || args.after_context.is_some()) {
        return Err(CliError::usage(
            "--context cannot be combined with --before-context or --after-context",
        ));
    }
    let root = std::env::current_dir().map_err(|error| CliError::io(Path::new("."), error))?;
    let options = LexicalSearchOptions {
        root: root.clone(),
        patterns,
        pattern_files: args.pattern_files.clone(),
        paths: args.rg_paths.clone(),
        limit: args.limit,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        globs: args.globs.clone(),
        insensitive_globs: args.iglobs.clone(),
        file_types: args.file_types.clone(),
        excluded_file_types: args.excluded_file_types.clone(),
        hidden: args.hidden,
        no_ignore: args.no_ignore,
        ignore_files: args.ignore_files.iter().map(PathBuf::from).collect(),
        max_depth: args
            .max_depth
            .map(usize::try_from)
            .transpose()
            .map_err(|_| CliError::usage("--max-depth is too large"))?,
        max_file_size_bytes: args
            .max_filesize
            .as_deref()
            .map(parse_byte_size)
            .transpose()?,
        follow: args.follow,
        modified_after: args
            .modified_after
            .as_deref()
            .map(|value| parse_modified_time(value, "--modified-after"))
            .transpose()?,
        modified_before: args
            .modified_before
            .as_deref()
            .map(|value| parse_modified_time(value, "--modified-before"))
            .transpose()?,
        fixed_strings: args.fixed_strings,
        ignore_case: args.ignore_case && !args.case_sensitive,
        smart_case: args.smart_case,
        word_regexp: args.word_regexp,
        before_context: before,
        after_context: after,
        max_count: args.max_count,
    };
    let searched = ZvecGrepService::new(service_options(
        None,
        None,
        args.api_key.clone(),
        None,
        args.model_cache.clone(),
        args.device,
    ))
    .rg_search(&options)?;
    let query = options.patterns.join(" | ");
    // Every field is set explicitly: a new `ContextDiagnostics` field
    // fails compilation here until its rg value is decided.
    let result = ZvecGrepContextResult {
        query,
        root: root.to_string_lossy().into_owned(),
        source: ContextSource::Rg,
        coverage: if searched.diagnostics.truncated {
            zg_core::service::types::ContextCoverage::RgTruncated
        } else {
            zg_core::service::types::ContextCoverage::RgExhaustive
        },
        workspace_index: None,
        items: searched.items,
        group_results: None,
        diagnostics: ContextDiagnostics {
            empty_reason: None,
            index: None,
            rg: serde_json::to_value(&searched.diagnostics).ok(),
            structure: None,
            timings: None,
        },
    };
    print_context_result(&result, Rendering::from(args.human), color);
    if let Some(missing) = searched.diagnostics.missing_paths {
        for path in missing {
            eprintln!("warning: path not found: {path}");
        }
    }
    Ok(())
}

#[cfg(test)]
// A panic in test code is just a test failure, so indexing in assertions needs no guard.
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn direct_rg_finds_fixture_hits() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn alpha() {}\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "nothing here\n").unwrap();
        let root = dir.path().to_owned();
        let options = LexicalSearchOptions {
            root,
            patterns: vec!["alpha".to_owned()],
            ..LexicalSearchOptions::default()
        };
        let service = ZvecGrepService::new(service_options(None, None, None, None, None, None));
        let searched = service.rg_search(&options).unwrap();
        assert_eq!(searched.items.len(), 1);
        assert_eq!(searched.items[0].file.relative_path, "a.rs");
        assert!(!searched.diagnostics.truncated);
    }
}

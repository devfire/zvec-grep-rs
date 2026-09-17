//! CLI shape tests: clap parsing snapshots and `args.ts`-verbatim messages.

use super::*;
use clap::Parser;

fn parse(argv: &[&str]) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(argv)
}

#[test]
fn query_snapshot() {
    let cli = parse(&[
        "zg",
        "query",
        "hello world",
        "--hybrid",
        "other",
        "--limit",
        "5",
        "--mode",
        "direct",
        "--glob",
        "*.rs",
        "--trace",
    ])
    .unwrap();
    let Command::Query(args) = cli.command.unwrap() else {
        panic!("expected query");
    };
    assert_eq!(args.queries, vec!["hello world"]);
    assert_eq!(args.hybrid, vec!["other"]);
    assert_eq!(args.limit, Some(5));
    assert_eq!(args.mode, Some(ClientModeArg::Direct));
    assert_eq!(args.globs, vec!["*.rs"]);
    assert!(args.trace);
    validate(&Cli {
        command: Some(Command::Query(args)),
    })
    .unwrap();
}

#[test]
fn rg_snapshot_with_short_flags() {
    let cli = parse(&["zg", "query", "--rg", "pattern", "-i", "-C", "2"]).unwrap();
    let Command::Query(args) = cli.command.unwrap() else {
        panic!("expected query");
    };
    assert!(args.rg && args.ignore_case);
    assert_eq!(args.context, Some(2));
    validate(&Cli {
        command: Some(Command::Query(args)),
    })
    .unwrap();
}

#[test]
fn rg_rejects_hybrid() {
    let cli = parse(&["zg", "query", "--rg", "x", "--hybrid", "y"]).unwrap();
    let error = validate(&cli).expect_err("--rg + --hybrid must fail");
    assert_eq!(
        error.to_string(),
        "--rg cannot be combined with --hybrid, --fts, or --vector"
    );
}

#[test]
fn rg_output_option_uses_ts_text() {
    let cli = parse(&["zg", "query", "--rg", "x", "--count"]).unwrap();
    let error = validate(&cli).expect_err("--count must fail");
    assert_eq!(
        error.to_string(),
        "--count changes rg output and cannot be used with managed --rg"
    );
}

#[test]
fn compat_flag_directs_to_the_tool() {
    let cli = parse(&["zg", "query", "--rg", "x", "--threads", "4"]).unwrap();
    let error = validate(&cli).expect_err("--threads must fail");
    assert!(error.to_string().contains("zvec_grep_rg"));
}

#[test]
fn compat_flag_requires_rg() {
    let cli = parse(&["zg", "query", "x", "--ignore-case"]).unwrap();
    let error = validate(&cli).expect_err("--ignore-case without --rg must fail");
    assert_eq!(
        error.to_string(),
        "--ignore-case can only be used with --rg"
    );
}

#[test]
fn discovery_flag_requires_index_or_rg() {
    let cli = parse(&["zg", "query", "x", "--hidden"]).unwrap();
    let error = validate(&cli).expect_err("--hidden without --rg must fail");
    assert_eq!(
        error.to_string(),
        "--hidden can only be used with index commands or zg query --rg"
    );
}

#[test]
fn removed_json_flag_uses_ts_text() {
    let cli = parse(&["zg", "query", "x", "--json"]).unwrap();
    let error = validate(&cli).expect_err("--json must fail");
    assert_eq!(
        error.to_string(),
        "--json has been removed; use the default agent markdown output or --human"
    );
}

#[test]
fn auth_shape_errors_are_verbatim() {
    let cli = parse(&["zg", "auth", "status"]).unwrap();
    validate(&cli).unwrap();
    let cli = parse(&["zg", "auth"]).unwrap();
    assert!(validate(&cli).is_err());
}

#[test]
fn server_shape_errors_are_verbatim() {
    let cli = parse(&["zg", "server", "run", "--stdio"]).unwrap();
    let error = validate(&cli).expect_err("run + --stdio must fail");
    assert_eq!(
        error.to_string(),
        "--stdio cannot be combined with a server action"
    );
    let cli = parse(&["zg", "server"]).unwrap();
    let error = validate(&cli).expect_err("bare server must fail");
    assert_eq!(
        error.to_string(),
        "zg server requires on, off, status, run, or --stdio"
    );
}

#[test]
fn config_and_index_shapes() {
    let cli = parse(&["zg", "config", "provider", "set", "qwen"]).unwrap();
    let error = validate(&cli).expect_err("missing --api-key must fail");
    assert_eq!(
        error.to_string(),
        "zg config provider set requires --api-key"
    );
    let cli = parse(&["zg", "index", "a", "b"]).unwrap();
    let error = validate(&cli).expect_err("two roots must fail");
    assert_eq!(error.to_string(), "zg index accepts at most one root path");
    let cli = parse(&["zg", "index", "--color", "always", "--no-color"]).unwrap();
    let error = validate(&cli).expect_err("color + no-color must fail");
    assert_eq!(
        error.to_string(),
        "zg index --color cannot be combined with --no-color"
    );
    let cli = parse(&["zg", "index", "--drop", "--rebuild"]).unwrap();
    let error = validate(&cli).expect_err("drop + rebuild must fail");
    assert_eq!(
        error.to_string(),
        "zg index --drop cannot be combined with indexing options"
    );
}

#[test]
fn index_nested_git_flag_parses_and_conflicts_with_drop() {
    let cli = parse(&["zg", "index", "--include-nested-git"]).unwrap();
    let Some(Command::Index(args)) = cli.command else {
        panic!("index command");
    };
    assert!(args.include_nested_git);
    let cli = parse(&["zg", "index"]).unwrap();
    let Some(Command::Index(args)) = cli.command else {
        panic!("index command");
    };
    assert!(!args.include_nested_git);
    let cli = parse(&["zg", "index", "--drop", "--include-nested-git", "."]).unwrap();
    let error = validate(&cli).expect_err("drop + include-nested-git must fail");
    assert_eq!(
        error.to_string(),
        "zg index --drop cannot be combined with indexing options"
    );
}

#[test]
fn byte_sizes_and_times_parse() {
    assert_eq!(parse_byte_size("512").unwrap(), 512);
    assert_eq!(parse_byte_size("10MB").unwrap(), 10 * 1024 * 1024);
    assert_eq!(parse_byte_size("2g").unwrap(), 2 * 1024 * 1024 * 1024);
    assert!(parse_byte_size("nope").is_err());
    assert_eq!(parse_modified_time("0", "--x").unwrap(), 0);
    assert!(parse_modified_time("not-a-date", "--x").is_err());
}

#[test]
fn target_splitting() {
    assert_eq!(
        split_targets(&["claude, codex".to_owned(), "qwen".to_owned()]),
        vec!["claude", "codex", "qwen"]
    );
}

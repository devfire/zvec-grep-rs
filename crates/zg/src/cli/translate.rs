//! Search-first (upstream) argv translation, applied before clap parses.
//!
//! The upstream CLI is flag/search-first: `zg <query>`, `zg --index`,
//! `zg --status`, `zg --install`, `zg --config`, `zg --auth`, and
//! `zg --server on|off|status|run|--stdio`. The Rust port's canonical
//! surface is subcommands (`zg query …`, `zg index …`, …). This layer
//! rewrites the upstream shapes onto the canonical ones so both spellings
//! run the identical command; it never invents new behaviour of its own.
//! See `docs/ts-divergence.md` for the full mapping and the warning rule.
//!
//! Rules, in order (only the first token after the program name is
//! inspected; everything after it is forwarded verbatim):
//!
//! 1. No first token (bare `zg`): unchanged — clap yields no subcommand
//!    and `main` prints the main help, exactly like the canonical form.
//! 2. `--index`, `--status`, `--install`, `--config`, `--auth`,
//!    `--server` (including `--flag=value`): replaced by the matching
//!    subcommand name; a `=value` becomes the next argv element.
//!    `zg --server --stdio` therefore becomes `zg server --stdio`, the
//!    shape MCP client configs written against upstream already emit.
//! 3. Any other leading `-` token (`--help`, `--version`, unknown flags):
//!    unchanged, so clap owns help/version/unknown-flag errors.
//! 4. A known subcommand name (including the `search` alias for `query`
//!    and the hidden removed `serve`): unchanged — canonical spellings
//!    keep working untouched.
//! 5. Anything else: a search. `query` is inserted, so `zg hello world`
//!    parses exactly as `zg query hello world` (each word a positional
//!    query, search flags after the words preserved). If that first word
//!    looks like a misspelled command (see
//!    [`command_lookalike_warning`]), a warning is attached for `main` to
//!    print to stderr; the search still runs.

use std::ffi::OsString;

/// Subcommand names the canonical clap tree accepts as a first token.
/// `search` is the upstream/legacy alias of `query`; `serve` is the
/// hidden removed alias that errors with its migration message.
const KNOWN_COMMANDS: &[&str] = &[
    "query",
    "search",
    "index",
    "status",
    "install",
    "uninstall",
    "config",
    "auth",
    "server",
    "help",
    "version",
    "completions",
    "serve",
];

/// Upstream leading flags and the subcommand each rewrites to.
const FLAG_COMMANDS: &[(&str, &str)] = &[
    ("--index", "index"),
    ("--status", "status"),
    ("--install", "install"),
    ("--config", "config"),
    ("--auth", "auth"),
    ("--server", "server"),
];

/// Argv after translation, plus an optional stderr warning for `main`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslatedArgv {
    /// Argv (program name first) ready for `Cli::try_parse_from`.
    pub argv: Vec<OsString>,
    /// Warning to print to stderr before dispatch, if the first search
    /// word looked like a misspelled command name.
    pub warning: Option<String>,
}

/// Translates upstream search-first argv onto the canonical subcommand
/// argv. `argv` includes the program name at index 0, as from
/// `std::env::args_os`. Non-UTF-8 or missing arguments pass through
/// untouched (clap reports any problem with them).
#[must_use]
pub fn translate_argv(argv: Vec<OsString>) -> TranslatedArgv {
    let passthrough = |argv: Vec<OsString>| TranslatedArgv {
        argv,
        warning: None,
    };
    let Some(first) = argv.get(1).and_then(|arg| arg.to_str()) else {
        return passthrough(argv);
    };
    let first = first.to_owned();

    // Rule 2: upstream leading flag -> subcommand.
    for (flag, command) in FLAG_COMMANDS {
        if first == *flag {
            return replace_first(argv, command, None);
        }
        if let Some(value) = first
            .strip_prefix(flag)
            .and_then(|rest| rest.strip_prefix('='))
        {
            return replace_first(argv, command, Some(value.to_owned()));
        }
    }

    // Rule 3: any other flag is clap's business (--help/--version/errors).
    if first.starts_with('-') {
        return passthrough(argv);
    }

    // Rule 4: canonical subcommand spellings are untouched.
    if KNOWN_COMMANDS.contains(&first.as_str()) {
        return passthrough(argv);
    }

    // Rule 5: search-first. Insert `query`; forward the rest verbatim.
    let warning = command_lookalike_warning(&first);
    let mut translated = Vec::with_capacity(argv.len() + 1);
    match argv.split_first() {
        Some((program, rest)) => {
            translated.push(program.clone());
            translated.push(OsString::from("query"));
            translated.extend(rest.iter().cloned());
        }
        // Unreachable: `argv.get(1)` succeeded above, so a head exists.
        // Return the input rather than panicking on a fallible path.
        None => return passthrough(argv),
    }
    TranslatedArgv {
        argv: translated,
        warning,
    }
}

/// Rewrites `argv[1]` (an upstream flag) to `command`, expanding an
/// optional `=value` into its own following element.
fn replace_first(argv: Vec<OsString>, command: &str, value: Option<String>) -> TranslatedArgv {
    let extra = usize::from(value.is_some());
    let mut translated = Vec::with_capacity(argv.len() + extra);
    match argv.split_first() {
        Some((program, rest)) => {
            translated.push(program.clone());
            translated.push(OsString::from(command));
            if let Some(value) = value {
                translated.push(OsString::from(value));
            }
            // `rest` starts with the flag itself; forward only what
            // followed it.
            if let Some((_, forwarded)) = rest.split_first() {
                translated.extend(forwarded.iter().cloned());
            }
        }
        None => {
            return TranslatedArgv {
                argv,
                warning: None,
            };
        }
    }
    TranslatedArgv {
        argv: translated,
        warning: None,
    }
}

/// Upstream-#105 warning: a first search word that is a proper prefix
/// (at least three characters) of a known command name looks like a
/// misspelled command (`zg quer …`, `zg stat …`), not a deliberate
/// search. Only the prefix rule is used — deliberately: edit-distance
/// guesses would nag genuine one-word searches (`zg stats` is a real
/// query, and it is also not a prefix of any command). The warning never
/// changes the outcome; the tokens still run as a search, and spelling
/// the canonical `zg query …` form suppresses it.
fn command_lookalike_warning(first: &str) -> Option<String> {
    if first.chars().count() < 3 {
        return None;
    }
    let matches: Vec<&str> = KNOWN_COMMANDS
        .iter()
        .copied()
        .filter(|command| command.len() > first.len() && command.starts_with(first))
        .collect();
    match matches.as_slice() {
        [] => None,
        [command] => Some(format!(
            "warning: '{first}' looks like the '{command}' command; treating it as a \
             search query (use 'zg {command} ...' for the command, or 'zg query \
             {first} ...' to search without this warning)"
        )),
        many => Some(format!(
            "warning: '{first}' looks like a command name ({}); treating it as a \
             search query (use 'zg query {first} ...' to search without this warning)",
            many.join(", ")
        )),
    }
}

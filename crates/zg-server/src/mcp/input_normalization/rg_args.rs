//! Managed-rg argv grammar: `rg_query_from_input` plus the accepted-flag
//! parsers that lower argv onto `ParsedRgCommand`.

use crate::backend::RgQuery;
use crate::mcp::error::McpError;
use crate::mcp::schemas::{RgInput, parse_root};
use crate::root_runtime::RootKey;

use super::paths::assert_root_scoped;
use super::rg_scan::{
    apply_rg_compat, is_rg_engine_option, is_rg_output_option, is_short_output_option,
    reject_inline, reject_stdin_pattern_file, scan_rg_command, split_head_suffix,
};

/// Parses an `rg` command string into a backend lexical query, mirroring
/// `parseManagedRgCommand` + `contextOptionsFromRgInput`.
///
/// # Errors
///
/// Returns [`McpError::InvalidParams`] when the root is invalid, the rg command is
/// malformed or unsupported, or a path escapes the root.
pub fn rg_query_from_input(input: &RgInput) -> Result<(RootKey, RgQuery), McpError> {
    let root = parse_root(&input.root)?;
    let parsed = parse_managed_rg_command(&input.command)?;
    for path in parsed
        .paths
        .iter()
        .chain(parsed.ignore_files.iter())
        .chain(parsed.pattern_files.iter())
    {
        assert_root_scoped(root.as_str(), path)?;
    }
    Ok((
        root,
        RgQuery {
            patterns: parsed.patterns,
            paths: parsed.paths,
            limit: parsed.limit,
            fixed_strings: parsed.fixed_strings,
            ignore_case: parsed.ignore_case,
            max_count: parsed.max_count,
            pattern_files: parsed.pattern_files,
            globs: parsed.globs,
            insensitive_globs: parsed.insensitive_globs,
            file_types: parsed.file_types,
            excluded_file_types: parsed.excluded_file_types,
            hidden: parsed.hidden,
            no_ignore: parsed.no_ignore,
            ignore_files: parsed.ignore_files,
            max_depth: parsed.max_depth,
            max_file_size_bytes: parsed.max_file_size_bytes,
            smart_case: parsed.smart_case,
            word_regexp: parsed.word_regexp,
            whole_line: parsed.whole_line,
            before_context: parsed.before_context,
            after_context: parsed.after_context,
        },
    ))
}

/// Parsed `rg` argv before root scoping.
#[derive(Default)]
struct ParsedRgCommand {
    patterns: Vec<String>,
    paths: Vec<String>,
    limit: Option<usize>,
    fixed_strings: bool,
    ignore_case: bool,
    smart_case: bool,
    word_regexp: bool,
    whole_line: bool,
    max_count: Option<usize>,
    pattern_files: Vec<String>,
    globs: Vec<String>,
    insensitive_globs: Vec<String>,
    file_types: Vec<String>,
    excluded_file_types: Vec<String>,
    hidden: bool,
    no_ignore: bool,
    ignore_files: Vec<String>,
    max_depth: Option<usize>,
    max_file_size_bytes: Option<u64>,
    before_context: usize,
    after_context: usize,
}

fn parse_managed_rg_command(command: &str) -> Result<ParsedRgCommand, McpError> {
    let tokens = scan_rg_command(command)?;
    let (argv, limit) = split_head_suffix(&tokens)?;
    let argv = apply_rg_compat(argv);
    if !argv.first().is_some_and(|first| first == "rg") {
        return Err(McpError::invalid_params(
            "rg command must start with \"rg\".",
        ));
    }
    if argv.len() == 1 {
        return Err(McpError::invalid_params("rg command requires a pattern."));
    }
    let rest = argv
        .get(1..)
        .ok_or_else(|| McpError::invalid_params("rg command requires a pattern."))?;
    let mut parsed = parse_rg_argv(rest)?;
    parsed.command.limit = limit;
    // Positional pattern unless `-e`/`-f` supplied (mirrors
    // `normalizeManagedRgInput`); patterns are trimmed and empties dropped.
    if parsed.command.patterns.is_empty() && parsed.command.pattern_files.is_empty() {
        let Some(first) = parsed.positionals().first().cloned() else {
            return Err(McpError::invalid_params(
                "rg command requires a non-empty pattern.",
            ));
        };
        parsed.command.patterns = vec![first];
        parsed.positional_start = 1;
    }
    parsed.command.patterns = std::mem::take(&mut parsed.command.patterns)
        .into_iter()
        .map(|pattern| pattern.trim().to_owned())
        .filter(|pattern| !pattern.is_empty())
        .collect();
    if parsed.command.patterns.is_empty() && parsed.command.pattern_files.is_empty() {
        return Err(McpError::invalid_params(
            "rg command requires a non-empty pattern.",
        ));
    }
    Ok(parsed.into_command())
}

/// Raw argv parse with positional tracking.
struct RawRgParse {
    command: ParsedRgCommand,
    positionals: Vec<String>,
    positional_start: usize,
}

impl RawRgParse {
    fn positionals(&self) -> &[String] {
        &self.positionals
    }

    fn into_command(mut self) -> ParsedRgCommand {
        let start = self.positional_start.min(self.positionals.len());
        self.command.paths = self.positionals.split_off(start);
        self.command
    }
}

fn parse_rg_argv(argv: &[String]) -> Result<RawRgParse, McpError> {
    let mut command = ParsedRgCommand::default();
    let mut positionals = Vec::new();
    let mut index = 0;
    let mut end_of_flags = false;
    while index < argv.len() {
        let Some(token) = argv.get(index) else {
            break;
        };
        if end_of_flags || !token.starts_with('-') || token == "-" {
            if token == "-" {
                return Err(McpError::invalid_params(
                    "rg command cannot read patterns from stdin.",
                ));
            }
            // `---x` tokens are patterns, never flags (mirrors the TS
            // `--` insertion for triple-hyphen tokens).
            positionals.push(token.clone());
            index += 1;
            continue;
        }
        if token == "--" {
            end_of_flags = true;
            index += 1;
            continue;
        }
        if let Some(name) = token.strip_prefix("--") {
            index = parse_long_flag(argv, index, name, &mut command)?;
            continue;
        }
        index = parse_short_group(argv, index, &mut command)?;
    }
    Ok(RawRgParse {
        command,
        positionals,
        positional_start: 0,
    })
}

/// Long flags mappable onto the in-process engine; anything else errors
/// with the TS messages.
fn parse_long_flag(
    argv: &[String],
    index: usize,
    name: &str,
    command: &mut ParsedRgCommand,
) -> Result<usize, McpError> {
    let (flag, inline) = match name.split_once('=') {
        Some((flag, value)) => (flag, Some(value)),
        None => (name, None),
    };
    // Value flags accept `--flag value` and `--flag=value`.
    let value_of = |flag: &str| -> Result<String, McpError> {
        if let Some(inline) = inline {
            return Ok(inline.to_owned());
        }
        argv.get(index + 1).cloned().ok_or_else(|| {
            McpError::invalid_params(format!("rg command option \"--{flag}\" requires a value."))
        })
    };
    // True when the value came from the next token (consume it too).
    let consumed_next = inline.is_none();
    match flag {
        "regexp" => {
            command.patterns.push(value_of("regexp")?);
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "file" => {
            let path = value_of("file")?;
            reject_stdin_pattern_file(&path)?;
            command.pattern_files.push(path);
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "glob" => {
            command.globs.push(value_of("glob")?);
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "iglob" => {
            command.insensitive_globs.push(value_of("iglob")?);
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "type" => {
            command.file_types.push(value_of("type")?);
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "type-not" => {
            command.excluded_file_types.push(value_of("type-not")?);
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "ignore-file" => {
            command.ignore_files.push(value_of("ignore-file")?);
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "max-count" => {
            command.max_count = Some(parse_non_negative("--max-count", &value_of("max-count")?)?);
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "max-depth" => {
            command.max_depth = Some(parse_non_negative("--max-depth", &value_of("max-depth")?)?);
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "max-filesize" => {
            command.max_file_size_bytes = Some(parse_byte_size(
                "--max-filesize",
                &value_of("max-filesize")?,
            )?);
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "after-context" => {
            command.after_context =
                parse_non_negative("--after-context", &value_of("after-context")?)?;
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "before-context" => {
            command.before_context =
                parse_non_negative("--before-context", &value_of("before-context")?)?;
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "context" => {
            let lines = parse_non_negative("--context", &value_of("context")?)?;
            command.before_context = lines;
            command.after_context = lines;
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "fixed-strings" => {
            reject_inline("fixed-strings", inline)?;
            command.fixed_strings = true;
            Ok(index + 1)
        }
        "no-fixed-strings" => {
            reject_inline("no-fixed-strings", inline)?;
            command.fixed_strings = false;
            Ok(index + 1)
        }
        "ignore-case" => {
            reject_inline("ignore-case", inline)?;
            command.ignore_case = true;
            Ok(index + 1)
        }
        "case-sensitive" => {
            reject_inline("case-sensitive", inline)?;
            command.ignore_case = false;
            command.smart_case = false;
            Ok(index + 1)
        }
        "smart-case" => {
            reject_inline("smart-case", inline)?;
            command.smart_case = true;
            Ok(index + 1)
        }
        "word-regexp" => {
            reject_inline("word-regexp", inline)?;
            command.word_regexp = true;
            Ok(index + 1)
        }
        "line-regexp" => {
            reject_inline("line-regexp", inline)?;
            command.whole_line = true;
            Ok(index + 1)
        }
        "hidden" => {
            reject_inline("hidden", inline)?;
            command.hidden = true;
            Ok(index + 1)
        }
        "no-ignore" => {
            reject_inline("no-ignore", inline)?;
            command.no_ignore = true;
            Ok(index + 1)
        }
        "files-with-matches" => {
            // Stripped by the compat layer before parsing; reaching here
            // means it arrived quoted oddly — treat like `-l`.
            reject_inline("files-with-matches", inline)?;
            command.max_count = Some(1);
            Ok(index + 1)
        }
        "follow" => Err(McpError::invalid_params(
            "rg command option \"--follow\" is not supported by the MCP tool.",
        )),
        "recursive" | "line-number" | "with-filename" => {
            reject_inline(flag, inline)?;
            Ok(index + 1)
        }
        _ if is_rg_output_option(flag) => Err(McpError::invalid_params(format!(
            "--{flag} changes rg output and cannot be used with managed --rg"
        ))),
        _ if is_rg_engine_option(flag) => Err(McpError::invalid_params(format!(
            "rg command option \"--{flag}\" is not supported by the MCP tool."
        ))),
        _ => Err(McpError::invalid_params(format!(
            "Unsupported --rg option: --{flag}"
        ))),
    }
}

/// Short-flag groups (`-in`, `-em1`): booleans combine, value flags take
/// the inline remainder or the next token (mirrors
/// `readShortOptionValue`).
fn parse_short_group(
    argv: &[String],
    index: usize,
    command: &mut ParsedRgCommand,
) -> Result<usize, McpError> {
    let Some(token) = argv.get(index) else {
        return Err(McpError::invalid_params("rg command requires a pattern."));
    };
    let bytes = token.as_bytes();
    let mut offset = 1;
    while offset < bytes.len() {
        let Some(short_byte) = bytes.get(offset) else {
            break;
        };
        let short = *short_byte as char;
        let flag = format!("-{short}");
        match short {
            'n' | 'H' => {
                // Display no-ops (mirrors the TS compatibility marking).
                offset += 1;
            }
            'F' => {
                command.fixed_strings = true;
                offset += 1;
            }
            'i' => {
                command.ignore_case = true;
                offset += 1;
            }
            's' => {
                command.ignore_case = false;
                command.smart_case = false;
                offset += 1;
            }
            'S' => {
                command.smart_case = true;
                offset += 1;
            }
            'w' => {
                command.word_regexp = true;
                offset += 1;
            }
            'x' => {
                command.whole_line = true;
                offset += 1;
            }
            'l' => {
                // Stripped by the compat layer; a group remainder means
                // `-l` arrived inside a group like `-il`.
                command.max_count = Some(1);
                offset += 1;
            }
            'L' => {
                return Err(McpError::invalid_params(
                    "rg command option \"--follow\" is not supported by the MCP tool.",
                ));
            }
            'e' | 'g' | 't' | 'T' | 'f' | 'm' | 'A' | 'B' | 'C' => {
                let inline = token[offset + 1..].to_owned();
                let consumed = usize::from(inline.is_empty());
                let value = if inline.is_empty() {
                    argv.get(index + 1).cloned().ok_or_else(|| {
                        McpError::invalid_params(format!(
                            "rg command option \"{flag}\" requires a value."
                        ))
                    })?
                } else {
                    inline
                };
                match short {
                    'e' => command.patterns.push(value),
                    'g' => command.globs.push(value),
                    't' => command.file_types.push(value),
                    'T' => command.excluded_file_types.push(value),
                    'f' => {
                        reject_stdin_pattern_file(&value)?;
                        command.pattern_files.push(value);
                    }
                    'm' => command.max_count = Some(parse_non_negative(&flag, &value)?),
                    'A' => command.after_context = parse_non_negative(&flag, &value)?,
                    'B' => command.before_context = parse_non_negative(&flag, &value)?,
                    _ => {
                        let lines = parse_non_negative(&flag, &value)?;
                        command.before_context = lines;
                        command.after_context = lines;
                    }
                }
                return Ok(index + 1 + consumed);
            }
            _ => {
                if is_short_output_option(short) {
                    return Err(McpError::invalid_params(format!(
                        "{flag} changes rg output and cannot be used with managed --rg"
                    )));
                }
                return Err(McpError::invalid_params(format!(
                    "Unsupported --rg option: {flag}"
                )));
            }
        }
    }
    Ok(index + 1)
}

fn parse_non_negative(option: &str, value: &str) -> Result<usize, McpError> {
    if value.is_empty() || !value.chars().all(|char| char.is_ascii_digit()) {
        return Err(McpError::invalid_params(format!(
            "{option} requires a non-negative integer"
        )));
    }
    value
        .parse::<usize>()
        .map_err(|_| McpError::invalid_params(format!("{option} requires a non-negative integer")))
}

/// Byte sizes with `K/M/G/T` suffixes, mirroring TS `parseByteSize`.
fn parse_byte_size(option: &str, value: &str) -> Result<u64, McpError> {
    let trimmed = value.trim();
    let (digits, suffix) = match trimmed.chars().last() {
        Some(last) if last.is_ascii_alphabetic() => (
            &trimmed[..trimmed.len() - 1],
            Some(last.to_ascii_uppercase()),
        ),
        _ => (trimmed, None),
    };
    let amount: u64 = digits.parse().map_err(|_| {
        McpError::invalid_params(format!(
            "{option} requires bytes or an integer K/M/G/T size"
        ))
    })?;
    let multiplier: u64 = match suffix {
        None => 1,
        Some('K') => 1024,
        Some('M') => 1024 * 1024,
        Some('G') => 1024 * 1024 * 1024,
        Some('T') => 1024 * 1024 * 1024 * 1024,
        Some(_) => {
            return Err(McpError::invalid_params(format!(
                "{option} requires bytes or an integer K/M/G/T size"
            )));
        }
    };
    amount
        .checked_mul(multiplier)
        .ok_or_else(|| McpError::invalid_params(format!("{option} is too large")))
}

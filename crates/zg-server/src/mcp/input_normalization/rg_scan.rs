//! Managed-rg front-end validation: shell tokenizer, `| head` suffix,
//! `-l` compatibility, and the rejection policy for output/engine options,
//! `stdin` patterns, and inline values on flag-only options.

use crate::mcp::error::McpError;

pub(crate) fn reject_inline(flag: &str, inline: Option<&str>) -> Result<(), McpError> {
    if inline.is_some() {
        return Err(McpError::invalid_params(format!(
            "rg command option \"--{flag}\" takes no value."
        )));
    }
    Ok(())
}

pub(crate) fn reject_stdin_pattern_file(path: &str) -> Result<(), McpError> {
    if path == "-" {
        return Err(McpError::invalid_params(
            "rg command cannot read patterns from stdin.",
        ));
    }
    Ok(())
}

/// Output-changing options, mirroring `MANAGED_RG_OUTPUT_OPTIONS` (plus
/// the short aliases the TS short switch maps to output errors).
pub(crate) fn is_rg_output_option(flag: &str) -> bool {
    matches!(
        flag,
        "count"
            | "count-matches"
            | "files"
            | "files-with-matches"
            | "files-without-match"
            | "column"
            | "byte-offset"
            | "no-column"
            | "colors"
            | "color"
            | "context-separator"
            | "field-context-separator"
            | "field-match-separator"
            | "json"
            | "heading"
            | "no-heading"
            | "no-filename"
            | "no-line-number"
            | "only-matching"
            | "passthru"
            | "path-separator"
            | "quiet"
            | "pretty"
            | "replace"
            | "stats"
            | "trim"
            | "null"
            | "no-messages"
            | "vimgrep"
    )
}

/// Short aliases for output-changing options.
pub(crate) fn is_short_output_option(short: char) -> bool {
    matches!(short, 'c' | 'o' | 'O' | 'q' | 'p' | '0' | 'N' | 'P')
}

/// Real-rg engine/behavior flags accepted by TS (forwarded to the rg
/// binary) but unmappable onto the in-process engine.
pub(crate) fn is_rg_engine_option(flag: &str) -> bool {
    matches!(
        flag,
        "auto-hybrid-regex"
            | "binary"
            | "crlf"
            | "invert-match"
            | "mmap"
            | "multiline"
            | "multiline-dotall"
            | "no-crlf"
            | "no-ignore-dot"
            | "no-ignore-files"
            | "no-ignore-global"
            | "no-ignore-parent"
            | "no-ignore-vcs"
            | "no-config"
            | "no-mmap"
            | "no-multiline"
            | "no-search-zip"
            | "pcre2"
            | "one-file-system"
            | "search-zip"
            | "stop-on-nonmatch"
            | "text"
            | "unicode"
            | "no-unicode"
            | "glob-case-insensitive"
            | "dfa-size-limit"
            | "encoding"
            | "engine"
            | "max-columns"
            | "regex-size-limit"
            | "threads"
            | "hidden" // handled above; listed for exhaustiveness of `--no-` forms below
            | "no-hidden"
            | "unrestricted"
            | "no-messages"
            | "sort"
            | "sortr"
    )
}

/// Shell-ish tokenizer mirroring `scanManagedRgCommand`: single/double
/// quotes, backslash escapes (non-Windows behavior), `|`/`>` splitting,
/// and rejection of NUL, newlines, shell operators, and expansions.
pub(crate) fn scan_rg_command(command: &str) -> Result<Vec<String>, McpError> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut escaping = false;
    let chars: Vec<char> = command.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let Some(char) = chars.get(index).copied() else {
            break;
        };
        if char == '\0' {
            return Err(McpError::invalid_params(
                "rg command cannot contain NUL characters.",
            ));
        }
        if char == '\n' || char == '\r' {
            return Err(McpError::invalid_params(
                "rg command must be a single command on one line.",
            ));
        }
        if escaping {
            token.push(char);
            started = true;
            escaping = false;
            index += 1;
            continue;
        }
        if quote == Some('\'') {
            if char == '\'' {
                quote = None;
            } else {
                token.push(char);
            }
            started = true;
            index += 1;
            continue;
        }
        if quote == Some('"') {
            if char == '"' {
                quote = None;
            } else if char == '\\' {
                let next = chars.get(index + 1).copied();
                match next {
                    None => {
                        return Err(McpError::invalid_params(
                            "rg command ends with an incomplete escape.",
                        ));
                    }
                    Some(next) if matches!(next, '"' | '\\' | '$' | '`') => {
                        token.push(next);
                        index += 1;
                    }
                    _ => token.push(char),
                }
            } else if char == '`' || starts_expansion(&chars, index) {
                return Err(McpError::invalid_params(
                    "rg command does not support shell expansion.",
                ));
            } else {
                token.push(char);
            }
            started = true;
            index += 1;
            continue;
        }
        if char.is_whitespace() {
            if started {
                tokens.push(std::mem::take(&mut token));
                started = false;
            }
            index += 1;
            continue;
        }
        if char == '\'' || char == '"' {
            quote = Some(char);
            started = true;
            index += 1;
            continue;
        }
        if char == '\\' {
            // Windows shells treat backslashes as separators; everywhere
            // else they escape (mirrors the TS platform branch).
            if cfg!(windows) {
                token.push(char);
            } else {
                escaping = true;
            }
            started = true;
            index += 1;
            continue;
        }
        if char == '|' || char == '>' {
            if started {
                tokens.push(std::mem::take(&mut token));
                started = false;
            }
            tokens.push(char.to_string());
            index += 1;
            continue;
        }
        if matches!(char, '&' | ';' | '<' | '(' | ')') {
            return Err(McpError::invalid_params(format!(
                "rg command does not support shell operator {char:?}."
            )));
        }
        if char == '`' || starts_expansion(&chars, index) {
            return Err(McpError::invalid_params(
                "rg command does not support shell expansion.",
            ));
        }
        token.push(char);
        started = true;
        index += 1;
    }
    if escaping {
        return Err(McpError::invalid_params(
            "rg command ends with an incomplete escape.",
        ));
    }
    if let Some(open) = quote {
        return Err(McpError::invalid_params(format!(
            "rg command has an unclosed {open} quote."
        )));
    }
    if started {
        tokens.push(token);
    }
    Ok(tokens)
}

fn starts_expansion(chars: &[char], index: usize) -> bool {
    chars.get(index).copied() == Some('$')
        && matches!(chars.get(index + 1).copied(), Some('(') | Some('{'))
}

/// Splits the trailing `| head` bound and the `2> /dev/null` red herring,
/// mirroring `normalizeManagedRgShellSuffix`.
pub(crate) fn split_head_suffix(
    tokens: &[String],
) -> Result<(Vec<String>, Option<usize>), McpError> {
    let mut argv = tokens.to_vec();
    let mut limit = None;
    if let Some(pipe) = argv.iter().rposition(|token| token == "|") {
        // `split_off` keeps `[..pipe]` in place; the suffix (pipe
        // included) is validated as the head bound.
        let suffix = argv.split_off(pipe);
        let head_args = suffix.get(1..).ok_or_else(|| {
            McpError::invalid_params(
                "rg command only supports a trailing \"| head\", \"| head -N\", or \"| head -n N\" output bound.",
            )
        })?;
        limit = Some(parse_head_limit(head_args)?);
    }
    if matches!(argv.as_slice(), [.., a, b, c] if a.as_str() == "2" && b.as_str() == ">" && c.as_str() == "/dev/null")
    {
        argv.truncate(argv.len() - 3);
    }
    if argv.iter().any(|token| token == "|" || token == ">") {
        let operator = argv
            .iter()
            .find(|token| token.as_str() == "|" || token.as_str() == ">")
            .cloned()
            .unwrap_or_default();
        return Err(McpError::invalid_params(format!(
            "rg command does not support shell operator {operator:?}."
        )));
    }
    Ok((argv, limit))
}

/// `| head`, `| head -N`, `| head -n N` — the only supported suffix.
fn parse_head_limit(suffix: &[String]) -> Result<usize, McpError> {
    let unsupported = || {
        McpError::invalid_params(
            "rg command only supports a trailing \"| head\", \"| head -N\", or \"| head -n N\" output bound.",
        )
    };
    let raw = match suffix {
        [head] if head == "head" => "10".to_owned(),
        [head, count] if head == "head" && is_head_count(count) => count[1..].to_owned(),
        [head, flag, count] if head == "head" && flag == "-n" && is_digits(count) => count.clone(),
        _ => return Err(unsupported()),
    };
    let limit: usize = raw.parse().map_err(|_| {
        McpError::invalid_params("rg command head limit must be a positive safe integer.")
    })?;
    if limit == 0 {
        return Err(McpError::invalid_params(
            "rg command head limit must be a positive safe integer.",
        ));
    }
    Ok(limit)
}

fn is_head_count(token: &str) -> bool {
    token.starts_with('-')
        && token.len() > 1
        && token[1..].chars().all(|char| char.is_ascii_digit())
}

fn is_digits(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|char| char.is_ascii_digit())
}

/// `-l`/`--files-with-matches` becomes `--max-count 1`, mirroring
/// `normalizeManagedRgCompatibilityTokens`.
pub(crate) fn apply_rg_compat(argv: Vec<String>) -> Vec<String> {
    let mut tokens = Vec::with_capacity(argv.len());
    let mut files_with_matches = false;
    for token in &argv {
        if token == "-l" || token == "--files-with-matches" {
            files_with_matches = true;
            continue;
        }
        if token.starts_with('-')
            && !token.starts_with("--")
            && token.contains('l')
            && token.len() > 1
        {
            let stripped: String = token.chars().filter(|char| *char != 'l').collect();
            if stripped != "-" {
                tokens.push(stripped);
            }
            files_with_matches = true;
            continue;
        }
        tokens.push(token.clone());
    }
    if files_with_matches && !tokens.is_empty() {
        tokens.insert(1, "--max-count".to_owned());
        tokens.insert(2, "1".to_owned());
    }
    tokens
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use crate::mcp::input_normalization::rg_args::rg_query_from_input;
    use crate::mcp::schemas::RgInput;

    #[test]
    fn parses_basic_rg_command() {
        let (root, query) = rg_query_from_input(&RgInput {
            root: "/repo".to_owned(),
            command: "rg -i --glob '*.rs' 'fn main' src | head -5".to_owned(),
        })
        .unwrap();
        assert_eq!(root.as_str(), "/repo");
        assert_eq!(query.patterns, vec!["fn main"]);
        assert_eq!(query.paths, vec!["src"]);
        assert_eq!(query.limit, Some(5));
        assert!(query.ignore_case);
        assert_eq!(query.globs, vec!["*.rs"]);
    }

    #[test]
    fn rg_rejects_shell_operators_and_unknown_flags() {
        let command = |command: &str| RgInput {
            root: "/repo".to_owned(),
            command: command.to_owned(),
        };
        assert!(rg_query_from_input(&command("rg foo && rm")).is_err());
        assert!(rg_query_from_input(&command("rg --threads 4 foo")).is_err());
        assert!(rg_query_from_input(&command("rg --json foo")).is_err());
        assert!(rg_query_from_input(&command("rg --follow foo")).is_err());
        assert!(rg_query_from_input(&command("grep foo")).is_err());
        assert!(rg_query_from_input(&command("rg")).is_err());
    }

    #[test]
    fn rg_rejects_paths_outside_root() {
        let input = RgInput {
            root: "/repo".to_owned(),
            command: "rg foo ../outside".to_owned(),
        };
        assert!(rg_query_from_input(&input).is_err());
    }
}

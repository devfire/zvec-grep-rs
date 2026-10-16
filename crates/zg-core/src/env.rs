//! Upstream environment-variable overrides (#79).
//!
//! Pure parsers take the raw value so tests never mutate the process
//! environment; thin wrappers read `std::env` and warn on invalid
//! values exactly like upstream (`using automatic parallelism`).

use std::time::Duration;

/// Seconds before an idle workspace watcher/runtime is released;
/// `0` keeps watchers until the server stops.
pub const WATCHER_IDLE_TIMEOUT_ENV: &str = "ZVEC_GREP_WATCHER_IDLE_TIMEOUT_SECONDS";
/// Index-stage embedding concurrency (explicit option wins).
pub const INDEX_EMBEDDING_CONCURRENCY_ENV: &str = "ZVEC_GREP_INDEX_EMBEDDING_CONCURRENCY";
/// Legacy llama.cpp-only fallback for [`INDEX_EMBEDDING_CONCURRENCY_ENV`].
pub const LLAMA_CONTEXT_PARALLELISM_ENV: &str = "ZVEC_GREP_LLAMA_CONTEXT_PARALLELISM";
/// `1` keeps Metal residency sets; anything else enables the upstream
/// `GGML_METAL_NO_RESIDENCY=1` mitigation on macOS.
pub const METAL_KEEP_RESIDENCY_ENV: &str = "ZVEC_GREP_METAL_KEEP_RESIDENCY";

/// Upstream cap for llama.cpp / Transformers.js index concurrency.
pub const LOCAL_EMBEDDING_CONCURRENCY_CAP: usize = 8;

/// Parses a positive-integer concurrency (`0*[1-9][0-9]*`, trimmed).
#[must_use]
pub fn parse_positive_concurrency(raw: Option<&str>) -> Option<usize> {
    let value = raw?.trim();
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if value.bytes().all(|byte| byte == b'0') {
        return None;
    }
    value.parse::<usize>().ok().filter(|parsed| *parsed > 0)
}

/// Priority: explicit option, then index env, then (llama only) the
/// legacy llama env. Invalid env values yield `None` (automatic).
#[must_use]
pub fn resolve_index_embedding_concurrency(
    requested: Option<usize>,
    index_raw: Option<&str>,
    llama_raw: Option<&str>,
    is_llama: bool,
) -> Option<usize> {
    if let Some(requested) = requested.filter(|value| *value > 0) {
        return Some(requested);
    }
    if let Some(parsed) = parse_positive_concurrency(index_raw) {
        return Some(parsed);
    }
    if index_raw.is_some_and(|raw| !raw.trim().is_empty()) {
        // Present but invalid: upstream warns and stays automatic;
        // it does not fall through to the legacy variable.
        return None;
    }
    if is_llama {
        return parse_positive_concurrency(llama_raw);
    }
    None
}

/// Reads the index-concurrency env override, warning on invalid input.
#[must_use]
#[allow(clippy::print_stderr)] // Upstream warns on stderr verbatim.
pub fn index_embedding_concurrency_from_env(is_llama: bool) -> Option<usize> {
    let index_raw = std::env::var(INDEX_EMBEDDING_CONCURRENCY_ENV).ok();
    let llama_raw = std::env::var(LLAMA_CONTEXT_PARALLELISM_ENV).ok();
    let resolved = resolve_index_embedding_concurrency(
        None,
        index_raw.as_deref(),
        llama_raw.as_deref(),
        is_llama,
    );
    if resolved.is_none() {
        if let Some(raw) = index_raw.as_deref().filter(|raw| !raw.trim().is_empty()) {
            eprintln!(
                "zvec-grep warning: invalid {INDEX_EMBEDDING_CONCURRENCY_ENV}=\"{raw}\", using automatic parallelism."
            );
        } else if is_llama
            && let Some(raw) = llama_raw.as_deref().filter(|raw| !raw.trim().is_empty())
        {
            eprintln!(
                "zvec-grep warning: invalid {LLAMA_CONTEXT_PARALLELISM_ENV}=\"{raw}\", using automatic parallelism."
            );
        }
    }
    resolved
}

/// Parses watcher idle seconds; `0` is meaningful (never release).
#[must_use]
pub fn parse_watcher_idle_timeout(raw: Option<&str>) -> Option<Duration> {
    let value = raw?.trim();
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse::<u64>().ok().map(Duration::from_secs)
}

/// Reads [`WATCHER_IDLE_TIMEOUT_ENV`], warning on invalid input.
#[must_use]
#[allow(clippy::print_stderr)] // Upstream warns on stderr verbatim.
pub fn watcher_idle_timeout_from_env() -> Option<Duration> {
    let raw = std::env::var(WATCHER_IDLE_TIMEOUT_ENV).ok()?;
    let parsed = parse_watcher_idle_timeout(Some(&raw));
    if parsed.is_none() && !raw.trim().is_empty() {
        eprintln!(
            "zvec-grep warning: invalid {WATCHER_IDLE_TIMEOUT_ENV}=\"{raw}\", using the default idle timeout."
        );
    }
    parsed
}

/// True only for the exact upstream opt-in value `1`.
#[must_use]
pub fn metal_keep_residency(raw: Option<&str>) -> bool {
    raw == Some("1")
}

/// Applies the upstream Darwin Metal residency mitigation before the
/// llama backend initializes: unless the user opted into keeping
/// residency, default `GGML_METAL_NO_RESIDENCY` to `1` (never
/// overwriting an explicit user setting). No-op off macOS.
pub fn apply_metal_residency_mitigation() {
    #[cfg(target_os = "macos")]
    {
        if metal_keep_residency(std::env::var(METAL_KEEP_RESIDENCY_ENV).ok().as_deref()) {
            return;
        }
        if std::env::var_os("GGML_METAL_NO_RESIDENCY").is_none() {
            // SAFETY: the only value ever written is the fixed "1";
            // called once before `LlamaBackend::init` spawns Metal
            // work, and concurrent writers would write the same value.
            unsafe { std::env::set_var("GGML_METAL_NO_RESIDENCY", "1") };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positive_concurrency_parsing() {
        assert_eq!(parse_positive_concurrency(Some(" 007 ")), Some(7));
        assert_eq!(parse_positive_concurrency(Some("0")), None);
        assert_eq!(parse_positive_concurrency(Some("-1")), None);
        assert_eq!(parse_positive_concurrency(Some("1.5")), None);
        assert_eq!(parse_positive_concurrency(Some("")), None);
        assert_eq!(parse_positive_concurrency(None), None);
    }

    #[test]
    fn concurrency_priority_and_legacy_gating() {
        assert_eq!(
            resolve_index_embedding_concurrency(Some(3), Some("5"), Some("7"), true),
            Some(3)
        );
        assert_eq!(
            resolve_index_embedding_concurrency(None, Some("5"), Some("7"), true),
            Some(5)
        );
        assert_eq!(
            resolve_index_embedding_concurrency(None, None, Some("7"), true),
            Some(7)
        );
        assert_eq!(
            resolve_index_embedding_concurrency(None, None, Some("7"), false),
            None
        );
        // Invalid index env blocks legacy fallthrough (upstream).
        assert_eq!(
            resolve_index_embedding_concurrency(None, Some("x"), Some("7"), true),
            None
        );
    }

    #[test]
    fn watcher_timeout_allows_zero() {
        assert_eq!(
            parse_watcher_idle_timeout(Some("0")),
            Some(Duration::ZERO)
        );
        assert_eq!(
            parse_watcher_idle_timeout(Some("7200")),
            Some(Duration::from_secs(7200))
        );
        assert_eq!(parse_watcher_idle_timeout(Some("-1")), None);
    }

    #[test]
    fn metal_residency_is_exact_opt_in() {
        assert!(metal_keep_residency(Some("1")));
        assert!(!metal_keep_residency(Some("true")));
        assert!(!metal_keep_residency(None));
    }
}

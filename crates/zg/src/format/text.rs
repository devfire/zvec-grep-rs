//! Plain-text shaping helpers: scores, one-liners, truncation.
//!
//! Idiomatic ports, not byte copies, of the TS `oneLine`/`truncate`
//! helpers (see `docs/ts-divergence.md`). Pure builders: every function
//! is [`#[must_use]`] so a dropped return is a compiler warning, not a
//! silent no-op.

/// Formats a score like the TS agent view: integers plain, else 4dp.
#[must_use]
pub fn format_score(score: f64) -> String {
    if score.fract() == 0.0 && score.is_finite() {
        format!("{}", score.trunc() as i64)
    } else {
        format!("{score:.4}")
    }
}

/// Collapses whitespace runs, mirroring `oneLine`.
#[must_use]
pub fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Clips to `max` chars with an ellipsis, mirroring `truncate`.
#[must_use]
pub fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_owned();
    }
    let clipped: String = value.chars().take(max.saturating_sub(1)).collect();
    format!("{clipped}…")
}

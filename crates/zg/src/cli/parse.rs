//! Value parsers for flags clap cannot express: byte sizes, timestamps,
//! environment-variable names, and comma-separated target lists.

use crate::error::CliError;

/// Parses `--max-filesize` values: plain bytes or `K`/`M`/`G` suffixed
/// (`10MB`, `512k`). Mirrors `parseByteSize` in scale, not in message.
pub fn parse_byte_size(value: &str) -> Result<u64, CliError> {
    let invalid = || {
        CliError::usage(format!(
            "--max-filesize must be a byte size, got \"{value}\""
        ))
    };
    let trimmed = value.trim();
    let split = trimmed
        .char_indices()
        .find(|(_, marker)| marker.is_alphabetic())
        .map(|(index, _)| index);
    let (digits, suffix) = match split {
        Some(index) => trimmed.split_at(index),
        None => (trimmed, ""),
    };
    let factor: u64 = match suffix.trim().to_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1024,
        "m" | "mb" => 1024 * 1024,
        "g" | "gb" => 1024 * 1024 * 1024,
        _ => return Err(invalid()),
    };
    let (whole, fraction) = match digits.trim().split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (digits.trim(), ""),
    };
    if (whole.is_empty() && fraction.is_empty())
        || !fraction.chars().all(|marker| marker.is_ascii_digit())
    {
        return Err(invalid());
    }
    let whole: u64 = if whole.is_empty() {
        0
    } else {
        whole.parse().map_err(|_| invalid())?
    };
    let fraction_value: u128 = if fraction.is_empty() {
        0
    } else {
        fraction.parse().map_err(|_| invalid())?
    };
    let fraction_bytes = if fraction_value == 0 {
        0
    } else {
        let exponent = u32::try_from(fraction.len()).map_err(|_| invalid())?;
        let denominator = 10u128.checked_pow(exponent).ok_or_else(invalid)?;
        let scaled = fraction_value
            .checked_mul(u128::from(factor))
            .ok_or_else(invalid)?;
        u64::try_from(scaled / denominator).map_err(|_| invalid())?
    };
    whole
        .checked_mul(factor)
        .and_then(|bytes| bytes.checked_add(fraction_bytes))
        .ok_or_else(invalid)
}

/// Parses `--modified-after`/`--modified-before`: unix millis, RFC 3339,
/// `YYYY-MM-DD HH:MM:SS`, or `YYYY-MM-DD` (local midnight). Mirrors the
/// MCP `TimeInput` accepted set and message exactly.
pub fn parse_modified_time(value: &str, option: &str) -> Result<i64, CliError> {
    use chrono::TimeZone;
    let trimmed = value.trim();
    if !trimmed.is_empty() && trimmed.chars().all(|marker| marker.is_ascii_digit()) {
        return trimmed.parse::<i64>().map_err(|_| invalid_time(option));
    }
    if let Ok(date) = chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
        && let Some(midnight) = date.and_hms_opt(0, 0, 0)
        && let Some(local) = chrono::Local.from_local_datetime(&midnight).single()
    {
        return Ok(local.timestamp_millis());
    }
    if let Ok(moment) = chrono::DateTime::parse_from_rfc3339(trimmed) {
        return Ok(moment.timestamp_millis());
    }
    for format in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(trimmed, format)
            && let Some(local) = chrono::Local.from_local_datetime(&naive).single()
        {
            return Ok(local.timestamp_millis());
        }
    }
    Err(invalid_time(option))
}

fn invalid_time(option: &str) -> CliError {
    CliError::usage(format!(
        "{option} requires an epoch millisecond value or a parseable date"
    ))
}

/// Validates `--mcp-token-env` names, mirroring `parseEnvironmentVariable`.
pub fn parse_environment_variable(value: &str, option: &str) -> Result<String, CliError> {
    let valid = !value.is_empty()
        && value
            .chars()
            .all(|marker| marker.is_ascii_alphanumeric() || marker == '_')
        && !value
            .chars()
            .next()
            .is_some_and(|marker| marker.is_ascii_digit());
    if valid {
        return Ok(value.to_owned());
    }
    Err(CliError::usage(format!(
        "{option} must be a valid environment variable name"
    )))
}

/// Splits `--target` values on commas and whitespace, dropping empties.
pub fn split_targets(values: &[String]) -> Vec<String> {
    values
        .iter()
        .flat_map(|value| value.split([',', ' ', '\t', '\n']))
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect()
}

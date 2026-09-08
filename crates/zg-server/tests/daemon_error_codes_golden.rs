//! Golden daemon code registry test (M1).
//!
//! Renders every bare daemon code from live `DaemonError` values via
//! `zg_server::errors::all_codes`, sorts and dedupes them, and compares
//! against `tests/golden/daemon-error-codes.txt`.
//!
//! Update rule: adding a `DaemonError` variant without extending both
//! `all_codes()` and the golden file fails the build.

fn live_codes() -> Vec<String> {
    let mut codes: Vec<String> = zg_server::errors::all_codes()
        .iter()
        .map(|code| (*code).to_owned())
        .collect();
    codes.sort();
    codes.dedup();
    codes
}

#[test]
fn daemon_codes_match_golden_registry() {
    let expected: Vec<String> = include_str!("golden/daemon-error-codes.txt")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect();
    assert_eq!(live_codes(), expected);
}

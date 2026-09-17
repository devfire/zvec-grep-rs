# Task Completion Checklist

Before any code change, MUST read `skill://rust-defensive-programming` + `skill://advanced-rust-patterns` (gate defined in `mem:conventions`).

Before concluding any task or committing changes, verify the following in sequence:

1. Format Check: `cargo fmt --all --check`
2. Compilation & Types: `cargo check --workspace` (or relevant crate `cargo check -p <crate>`)
3. Unit & Integration Tests: `cargo test --workspace` (or `cargo test -p zg-core`)
4. Clippy Lints: `cargo clippy --workspace --all-targets` (+ `--all-features`; deny-level, must exit 0)
5. Rustdoc: `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`
6. Memory Graph: `serena memories check` (when memory entries or references are altered)

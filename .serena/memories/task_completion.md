# Task Completion Checklist

Before concluding any task or committing changes, verify the following in sequence:

1. Format Check: `cargo fmt --all --check`
2. Compilation & Types: `cargo check --workspace` (or relevant crate `cargo check -p <crate>`)
3. Unit & Integration Tests: `cargo test --workspace` (or `cargo test -p zg-core`)
4. Clippy Lints: `cargo clippy --workspace --all-targets` (must be warning-free or match project baseline)
5. Memory Graph: `serena memories check` (when memory entries or references are altered)

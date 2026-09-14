# Suggested Commands

## Build & Check
- `cargo check --workspace` — fast type/syntax check across all crates
- `cargo check -p zg-core` — check only core engine
- `cargo build --workspace` — compile workspace in debug mode
- `cargo build --release` — compile release binaries with thin LTO

## Testing
- `cargo test -p zg-core` — run unit tests in `zg-core` (default features stay hermetic, no C++ toolchain)
- `cargo test --workspace --all-features` — also cover `onnx`/`llama` backends
- `cargo test --workspace` — run test suite across all workspace crates

## Linting & Formatting
- `cargo clippy -p zg-core` — run Clippy lints on core library
- `cargo clippy --workspace --all-targets` — run Clippy on all crates and test targets (deny-level, must exit 0; also run with `--all-features`)
- `cargo fmt --all --check` — verify code formatting
- `cargo fmt --all` — format all files in-place

## Docs
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` — intra-doc links must resolve; never link private items, use literal code spans

## Serena Memory Verification
- `serena memories check` — verify memory graph integrity and references

# zvec-grep-rs

Rust port of [zvec-grep](https://github.com/zvec-ai/zvec-grep): local-first hybrid search (ripgrep + BM25 + vectors) for humans and agents. `unsafe` forbidden workspace-wide.

## Crates

- `zg-core` — engine: scan → extract → embed → index → hybrid search (`ZvecGrepService` facade).
- `zg-server` — daemon scaffolding: trace context, logger (HTTP/MCP not yet wired).
- `zg` — CLI (stub: prints `zg`; commands not yet ported).

## Prereqs

Rust 1.85+, `cargo`. No C++ toolchain needed (default features are pure Rust; `onnx`/`llama` backends are opt-in stubs).

## Build / test / lint

```bash
cargo build --workspace
cargo test -p zg-core --features test-support
cargo clippy --workspace -- -D warnings
```

## Use the engine

```rust
use std::path::Path;
use std::sync::Arc;
use zg_core::models::{EmbeddingModel, stub::StubEmbeddingModel};
use zg_core::service::facade::{CreateZvecGrepOptions, create_zvec_grep};
use zg_core::service::types::{ZvecGrepContextOptions, ZvecGrepIndexOptions};

let root = Path::new("/path/to/workspace");
let model: Arc<dyn EmbeddingModel> = Arc::new(StubEmbeddingModel::new(64));
let svc = create_zvec_grep(CreateZvecGrepOptions {
    root: Some(root.to_path_buf()),
    embedding_model: Some(model),
    ..Default::default()
});

svc.ensure_index(&ZvecGrepIndexOptions { root: Some(root), ..Default::default() })?;
let hits = svc.context(&ZvecGrepContextOptions {
    root: Some(root),
    query: Some("hello".to_owned()),
    fts: vec!["hello".to_owned()],
    ..Default::default()
})?;
```

## Status
Port in progress (phases 0–F done per `RUST_PORT_OF_ZVEC_GREP_PLAN.md`). `zg` CLI, server transport, and real embedding backends are not yet usable — build against `zg-core` directly.

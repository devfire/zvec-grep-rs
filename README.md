# zvec-grep-rs

Rust port of [zvec-grep](https://github.com/zvec-ai/zvec-grep): local-first hybrid search (ripgrep + BM25 + vectors) for humans and agents. `unsafe` forbidden workspace-wide (`[workspace.lints]`, `unsafe_code = "forbid"`).

## Crates

- `zg-core` — engine: scan → tree-sitter extract → embed → index → hybrid search, exposed through the sync `ZvecGrepService` facade (`create_zvec_grep`). Typed errors (`ModelError`, `StorageError`, …) with golden-tested `ZVEC_GREP.ENGINE.*` wire codes; newtypes for domain concepts (`ModelReference`, `RootKey`, `Generation`, …).
- `zg-server` — loopback daemon: one actor task per workspace root over a shared job scheduler and LRU embedding-model pool, filesystem watchers with debounced change sets, read-session cache with idle TTL, HTTP endpoint (loopback-only, bearer-token, body-cap guards) plus MCP over stdio and StreamableHTTP.
- `zg` — CLI at parity with the TS surface: `query` (incl. `--rg`, `--hybrid`, path filters, `--refresh`, `--mode direct|server|auto`), `index` (`--drop`/`--rebuild` + `--yes`), `status` (`--check-ready`), `config model|provider set`, `auth grant|status|revoke`, `server on|off|status|run|--stdio`, `install`/`uninstall` (IDE MCP configs), `help`, `version`, `completions`.

## What works

- **Multi-root daemon**: concurrent per-root actors (`DaemonBackend` + `RuntimeManager`), canonical `RootKey` resolution with alias dedupe, idle eviction (30 min default), cooperative cancellation, deterministic shutdown.
- **MCP**: 6 tools — `zvec_grep_search`, `zvec_grep_index`, `zvec_grep_index_drop`, `zvec_grep_rg`, `zvec_grep_index_status`, `zvec_grep_server_status` — with `agent|full` toolsets and validated input bounds at the boundary.
- **Embeddings**: pure-Rust `model2vec` (`local/potion-retrieval-32m`, dim 512), ONNX (`local/bge-small-en-v1.5`, `local/all-minilm-l6-v2`, dim 384; `--features onnx`), GGUF via llama.cpp (`local/embeddinggemma-300m` dim 768, `local/qwen3-embedding-0.6b` dim 1024; `--features llama`), and remote Qwen (`qwen/text-embedding-v4`, `qwen/qwen3.7-text-embedding` dim 1024, `qwen/qwen3-vl-embedding` dim 2560) requiring API key + workspace grant (fails closed without a permit). Vector-parity gate (cosine >= 0.999 vs TS goldens) covers all local backends.
- **Authorization**: workspace-scoped remote-embedding grants (`zg auth`), enforced in both direct and daemon paths.
- **Storage**: standalone — Rust indexes never share collections with the TS implementation; a TS-generation `files.zvec` is refused loudly (`STORAGE.FOREIGN_TS_INDEX_PRESENT`), never migrated.

## Local backends (opt-in)

`onnx` (ort) and `llama` (llama-cpp-2) cargo features, off by default so the default build stays pure Rust. Compiled out, those catalog entries resolve but `create_embedding_model` returns `ModelError::BackendUnavailable`. `llama` needs a C++ toolchain; both download models on first use.

## Prereqs

Rust 1.88+, `cargo` (`llama` feature additionally needs a C++ toolchain).

## Build / test / lint

```bash
cargo build --workspace
cargo build -p zg-core --features onnx,llama  # local backends
cargo test --workspace
cargo clippy --workspace --all-targets  # warning-free; deny lints (unwrap/expect/panic, indexing_slicing, …)
```

`Dockerfile` ships a portable multi-stage `zg` image (`$ORIGIN` rpath, no `LD_LIBRARY_PATH`).

Integration tests in `zg-server`/`zg` use `zg-core`'s `test-support` feature (`StubEmbeddingModel`: deterministic SHA-256-hash vectors — for tests only, not retrieval quality).

## Use

```bash
# Index and query through the daemon (default: auto spawns/uses it)
zg index /path/to/repo
zg query "where is retry logic implemented" --refresh wait

# In-process engine, no daemon
zg query "hello" --mode direct
zg status --check-ready
zg server run  # foreground daemon (HTTP + --stdio MCP)
```

```rust
use std::path::Path;
use zg_core::service::facade::{CreateZvecGrepOptions, create_zvec_grep};
use zg_core::service::types::{ZvecGrepContextOptions, ZvecGrepIndexOptions};

let root = Path::new("/path/to/workspace");
let svc = create_zvec_grep(CreateZvecGrepOptions {
    root: Some(root.to_path_buf()),
    embedding_model: None, // resolves the catalog default via factory
    ..Default::default()
)?;

svc.ensure_index(&ZvecGrepIndexOptions { root: Some(root), ..Default::default() })?;
let hits = svc.context(&ZvecGrepContextOptions {
    root: Some(root),
    query: Some("hello".to_owned()),
    fts: vec!["hello".to_owned()],
    ..Default::default()
})?;
```

## Status

Port phases 0, A–I per `RUST_PORT_OF_ZVEC_GREP_PLAN.md` are complete (facade, daemon, MCP, CLI, auth, standalone storage, ONNX + llama-cpp local backends). See `docs/ts-divergence.md` for deliberate divergences from the TS implementation.

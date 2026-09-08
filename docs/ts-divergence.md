# TS divergence log (M5)

Every module that intentionally diverges from its `../zvec-grep` namesake
records one line here: TS symbol, Rust shape, reason. Wire formats (on-disk
layout, HTTP routes, MCP tool names and bounds, CLI flags, error code
strings, auth prompt text) never diverge; only internal structure does.

## Models

- `factory.ts` returns rejected promises with string codes; `factory.rs`
  `create_embedding_model` / `plan_embedding_model` return
  `Result<_, ModelError>`. Reason: exhaustiveness-checked typed errors (M1)
  and `&ModelReference` instead of `&str` (M2); callers convert to
  `EngineError` at the boundary and the wire strings are unchanged.
- `factory.ts unsupportedCatalogEntry` arm; `factory.rs` returns
  `ModelError::BackendUnavailable` for llama-cpp/transformers-js until
  phases C/D land the `onnx`/`llama` cargo features (M7). Reason: stringly
  `NOT_IMPLEMENTED` cannot distinguish "no implementation" from "compiled
  out"; the code string is unchanged.
- `QwenTextEmbeddingV4Model` / `Qwen37TextEmbeddingModel` subclasses;
  `QwenTextModel::{V4, V37, Other}` with a base-prefix fallback for future
  catalog entries. Reason: no TS equivalent for unknown future models; Rust
  addition, wire codes for known models unchanged.
- `model2vec-worker-pool` (worker threads + message passing);
  `std::thread::scope` chunked batch embedding in `model2vec.rs`. Reason:
  no worker-thread equivalent needed — scoped threads share the borrow
  stack and preserve input order without channels.
- `service/types.ts EmbeddingModelOwnership { Owned, Borrowed }`; deleted.
  Reason: GC-language artifact — in Rust ownership is the type system and
  the discriminant next to an `Arc` invites impossible-state matches (M3).

## Storage

- `zvec.ts` FTS `tokenizerName: "jieba"`; `schema.rs` still uses
  `"standard"` for collections this port *writes*. Reason: writes work
  without a dictionary. *Reads* are a different story (phase E finding):
  genuine TS `index.zvec` collections carry a jieba FTS index that the
  native library refuses to open (`open_fts_indexers` failure) unless it
  sees a dictionary via `ZVEC_JIEBA_DICT_DIR` (`zvec-rust` 0.7 exposes no
  API for it; setting process environment from library code would need
  `unsafe`, forbidden by M8). So the dictionary is vendored
  (`crates/zg-core/vendor/jieba_dict/`, Apache-2.0, copied from
  `@zvec/bindings-linux-x64` with its license preserved alongside),
  `.cargo/config.toml` points `ZVEC_JIEBA_DICT_DIR` at it for dev/test,
  and production installs set the variable to the packaged dictionary.
  Without it, opening a TS-written entity collection fails with a hint
  naming the variable; `files.zvec` metadata (scalar-only, no FTS index)
  always opens. Proven: `tests/golden/ts-index/` opens under `zvec-rust`
  only with the variable set (a `standard`-tokenizer control collection
  opens without it).
- `deleteWorkspaceIndexStorage`; `layout.rs` branches file-vs-directory
  removal. Reason: `remove_dir_all` on `files.json` fails with `ENOTDIR`
  (bug fix, no behavior divergence).
- `CURRENT_INDEX_VERSION` (`types.ts`); `CURRENT_INDEX_VERSION = 2`
  with `LEGACY_TS_INDEX_VERSION = 1` (phase E, stance option 1).
  Reason: file metadata lives in `files.json` here instead of a second
  zvec collection, so a Rust-written manifest must be unmistakable to TS —
  TS rejects v2 with its own `WORKSPACE_INDEX.VERSION_MISMATCH` and a
  rebuild hint instead of silently diffing stale metadata. Rust accepts
  v1 (import path) and v2, rejects everything else; every `ensure_index`
  manifest write stamps v2, so a v1 index migrates on its next Rust run.
- `ZvecFileMetaStore` (zvec collection); `files.json` via `FileMetaStore`
  plus a one-way `files.zvec` importer (`storage/zvec/legacy_import.rs`).
  Reason: on open with no `files.json` (or a `files.zvec` newer than it —
  the TS-reindexed-after-Rust case), documents decode into `FileRecord`s
  and a *verified* import (every doc decoded, count matches collection
  stats) persists `files.json` and deletes `files.zvec`; anything else
  persists/deletes nothing and falls back to reindexing. Read-only opens
  never mutate. Fixture `tests/golden/ts-index/` is real TS output
  (`gen-fixture.mjs` drives `createZvecGrep` + `FakeEmbeddingModel`).
- `docToFileRecord` throwing readers; total readers with identical
  defaults. Reason: the TS readers never throw (missing string → `""`,
  missing number → `0`, missing bool → `false`, nullable number `<= 0` →
  `null`, malformed `entity_ids_json` → `[]`), so the Rust decoder
  defaults the same way and an imported `FileInfo` equals what TS
  `listFiles` reports — including the `0`-means-absent quirk. Fails only
  where Rust types cannot represent the value (unknown `kind`, negative
  unsigned); those docs are skipped with a `warn`, breaking verification
  (no delete) but not the open.
- `Range` snake_case fields; TS-shape fields (`startLine`, ... with
  snake_case tags). Reason: `range_json` is shared verbatim through the
  common `index.zvec` — the old spelling made every TS-written entity
  undecodable (hits silently dropped). Writes are now byte-identical to
  TS; reads additionally accept the old spelling via `alias`.
- Optional doc readers (`group`, `content_hash`, `heading_level`, ...);
  `codec.rs` maps absent fields to `None` via `has_field`. Reason: writers
  skip `None`, and reading an absent field errors — without this every
  recall hit fails to decode and search returns nothing (bug fix).

## Service

- `zvec-grep.ts` lives in `service/service.rs`; here it is
  `service/facade.rs`. Reason: `clippy::module_inception` (deny) forbids a
  module with its parent's name.
- `ZvecGrepService` methods are sync and the search method is named
  `context()` after the TS method and DTOs (the plan's "search" label).
  Reason: `EmbeddingModel::embed` is sync; async wrapping is phase G's
  `spawn_blocking` seam (M4/M6). Cancellation crosses via a poll thread
  tripping `CancelFlag`; the daemon wires `CancellationToken` properly.

## Deferred (accepted gaps, not silence)

- `ZvecGrepInfoResult` six-`Option` cluster stays until phase B, where the
  `ZvecGrepService` facade re-models it as `IndexState` with a serde wire
  adapter (M3).
- `StorageError` / `IndexingError` enums stay until their refactors; until
  then those layers construct `from_static` literals only (grep-gated), and
  the golden registry pins the typed `ModelError` + `codes` set.

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

- `zvec.ts` FTS `tokenizerName: "jieba"`; `schema.rs` uses `"standard"`.
  Reason: the server refuses `jieba` without a `jieba_dict_dir` that
  neither package ships — with `jieba` no collection can be created at
  all. `standard` segments space-separated text identically; CJK recall
  may differ. Revisit when dicts are vendored (phase E).
- `deleteWorkspaceIndexStorage`; `layout.rs` branches file-vs-directory
  removal. Reason: `remove_dir_all` on `files.json` fails with `ENOTDIR`
  (bug fix, no behavior divergence).
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

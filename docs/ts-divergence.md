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
  Reason: the builds are standalone and never share collections, so this
  port ships no dictionary and writes what opens everywhere. A
  TS-written `index.zvec` (jieba FTS index) does not open under
  `zvec-rust` without `ZVEC_JIEBA_DICT_DIR` — but since compat is not
  required, that variable is neither vendored nor configured; a
  `files.zvec` in the storage directory instead aborts open loudly with
  `STORAGE.FOREIGN_TS_INDEX_PRESENT` (never migrated, never adopted, and
  delete leaves it alone).
- `deleteWorkspaceIndexStorage`; `layout.rs` branches file-vs-directory
  removal. Reason: `remove_dir_all` on `files.json` fails with `ENOTDIR`
  (bug fix, no behavior divergence).
- Optional doc readers (`group`, `content_hash`, `heading_level`, ...);
  `codec.rs` maps absent fields to `None` via `has_field`. Reason: writers
  skip `None`, and reading an absent field errors — without this every
  recall hit fails to decode and search returns nothing (bug fix).

## Authorization (phase F)

- `authorization/*` throwing plain `Error`s (target/planner/store sites);
  `AuthError::{InvalidTarget, StoreFailed}` with new `AUTH.INVALID_TARGET` /
  `AUTH.STORE_FAILED` codes. Reason: M1 bans stringly errors; TS defines no
  wire code on these paths, so the codes are Rust additions and the
  `AUTH.REMOTE_EMBEDDING_REQUIRED` string is unchanged.
- `operation.ts` `AsyncLocalStorage` permit scope; thread-local
  `CURRENT_PERMIT` set by `with_remote_embedding_operation_permit`.
  Reason: the engine is sync — no async task context exists; the daemon
  (phase G) sets the permit around each `spawn_blocking` embed call.
- `target.ts` async `realpath` canonicalization; sync
  `std::fs::canonicalize` with lexical-absolute fallback. Reason: planning
  must work before the workspace directory exists; no tokio in `zg-core`.
- `store.ts` async fs; sync methods over `utils::lock` +
  `utils::json_io`. Reason: same — `zg-core` stays sync.
- Planner takes pre-normalized flags (`uses_vector`, `auto_update`,
  `freshness_wait`) instead of MCP `NormalizedSearchInput`. Reason: `zg-core`
  must not depend on the phase-H MCP layer; the MCP crate adapts.
- `prompt.ts` `value.length`/`slice` (UTF-16 units); Rust `chars()` counts.
  Reason: identical output for all realistic inputs; only non-BMP text
  without ASCII could differ by a clip boundary.
- Qwen `embed` checks the permit guard before input validation. Reason:
  fail-closed stance — a missing grant surfaces as `AUTH` even for
  otherwise-invalid inputs (TS order there is unobservable).
- `trace-context.ts` `AsyncLocalStorage`; thread-local set around each
  server request handler. Reason: same sync/async-boundary argument as the
  permit scope.

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

## Daemon (phase G)

- `backend.ts` thirteen `Map`/`Set`/flag fields shared across `await`;
  one actor task per root owning `RootRuntime` + coordinator + watcher +
  session cache as plain `&mut` state, `DaemonBackend` holding only
  key → sender + join handles. Reason: replicating TS's maps is lock soup
  (M6); per-root actors keep the same observable command surface with no
  cross-root shared mutable state.
- `AbortController`/`AbortSignal` in scheduler runs;
  `tokio_util::sync::CancellationToken` at the async edge,
  `CancelFlag` inside blocking bodies, adapted once in
  `job_scheduler::bridge_cancellation` (index runs cross as an owned
  `AbortCheck` probe instead). Reason: `spawn_blocking` cannot be aborted;
  shutdown awaits in-flight work (M6).
- `closePromise` chains; `RuntimeManager::close` awaiting actor joins +
  `JobScheduler::close` awaiting running jobs. Reason: no Rust analogue
  for a deferred promise field; explicit joins are deterministic.
- `model.dispose()`; dropping the last `Arc`. Reason: no dispose hook on
  the `EmbeddingModel` trait; eviction drops and destructors run.
- `watch-manager.ts` per-directory Linux watchers + resume timers; one
  native recursive `notify` watch with exclusion filtering. Reason: `notify`
  registers inotify watches natively per directory already; the quota
  problem TS works around does not apply.
- `server-controller.ts process.kill` signals; `/bin/kill` (`SIGTERM` then
  `SIGKILL`, Unix-only). Reason: `libc::kill` is `unsafe` and the
  workspace forbids `unsafe`; refusal/timeout surfaces as typed
  `ShutdownFailed` instead.
- `os.hostname()`; `$HOSTNAME` with `/proc/sys/kernel/hostname`
  fallback. Reason: no hostname dependency; same stability contract for
  lock comparison.
- Plain `Error`s for address-in-use / unknown-job / already-running /
  shutdown refusal; `DaemonError::{AddressInUse, UnknownJob,
  AlreadyRunning, ShutdownFailed}`. Reason: M1 bans stringly errors; the
  codes are Rust additions (recorded in the daemon golden registry).
- `root-runtime.ts` writer-context search routing; searches during an
  index read the last committed session (possibly stale). Reason: writer
  handoff would couple the scheduler task to actor state; staleness beats
  deadlock here and `index_status` still reports the live job.

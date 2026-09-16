# Rust port of zvec-grep — completion plan

## Context

`../zvec-grep/` is a TypeScript hybrid workspace-search system (1.5 MB / 130 `.ts` files under `src/`: tree-sitter extraction, ripgrep lexical, ONNX/GGUF/Qwen embeddings, dual-zvec-collection storage, loopback daemon, MCP server, `zg` CLI) plus a 60-file / ~21.6 kLOC test suite under `test/` and a retrieval benchmark under `benchmarks/swe-qa-bench/`.

The Rust port in this repo (`zvec-grep-rs`) stalled at one commit (`44c282e`, clean tree):

- `zg-core`: 79 files, ~21.5 kLOC, real bodies, 104 inline tests. Substantially written but **not wired** — `create_embedding_model` returns `NOT_IMPLEMENTED` on every arm, there is no service facade, and two unused imports plus a never-read field mark the seams where the remaining layers were meant to plug in.
- `zg-server`: a doc comment. `zg`: `println!("zg")`.
- Baseline reality check (verified, not assumed): `cargo test --workspace` passes (104 tests, 0 ignored); **`cargo clippy --workspace --all-targets -- -D warnings` fails with 15 findings**. The previous revision of this plan asserted that gate was clean and used it as every step's exit criterion. It is not. Phase 0 exists to fix that.

All async/MCP/CLI deps (tokio, axum 0.8, rmcp 0.6, clap 4, tracing, tracing-subscriber) are already declared in the workspace `Cargo.toml` and unused. This plan adds **three** new deps: `notify` (phase G), `ort` and `llama-cpp-2` (phases C/D, both behind off-by-default cargo features).

End state: `cargo test --workspace` green, `cargo clippy --workspace --all-targets` clean under a `[workspace.lints]` deny table, `zg` at parity with the TS CLI (10 commands), daemon HTTP + MCP stdio/HTTP at parity, embedding **vector parity** with TS proven by golden fixtures for all four local models, and a stated, tested stance on TS-written indexes.

## Engineering mandates (normative — these override any "mirror the TS" instruction)

These are acceptance criteria, not style preferences. A step is not done if it violates one. Reviewers reject on these without further justification.

### M1. No stringly-typed errors

Today: `EngineErrorCode(String)` built by `format!` at every call site — `EngineErrorCode::new(&format!("MODELS.{error_code_prefix}_MISSING_API_KEY"))` (`models/factory.rs:179,200`), and a `codes::extractor(suffix: &str)` escape hatch (`error.rs:97`). That is a runtime-assembled, allocating, typo-shaped error identity with no exhaustiveness checking — the single least idiomatic thing in the crate, and the previous plan froze it and propagated it into two more crates.

Required shape. The **wire string is the contract and does not change**; the internal representation stops being a `String`:

```rust
// error.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EngineErrorCode(&'static str);          // suffix only; no allocation, no format!

impl EngineErrorCode {
    pub const fn from_static(suffix: &'static str) -> Self { Self(suffix) }
    pub fn qualified(&self) -> String { format!("{ENGINE_ERROR_CODE_PREFIX}.{}", self.0) }
}
```

```rust
// models/error.rs
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("missing API key for {reference}")]
    MissingApiKey { reference: ModelReference, provider: Provider },
    #[error("embedding batch exceeds model limit")]
    BatchTooLarge { reference: ModelReference, size: usize, max: BatchSize },
    #[error("backend not compiled in")]
    BackendUnavailable { reference: ModelReference, backend: Backend },
    // ...
}

impl ModelError {
    pub const fn code(&self) -> EngineErrorCode { /* exhaustive match -> const codes */ }
}
```

- `EngineErrorCode::new(&format!(..))` is **banned**. Grep gate in CI: `grep -rn 'EngineErrorCode::new(&format!' crates/` must return nothing.
- `codes::extractor(suffix: &str)` is deleted; extractor codes become enum variants.
- Every domain gets its own error enum (`ModelError`, `StorageError`, `IndexingError`, `DaemonError`, `AuthError`, `McpError`) converging into `EngineError` via `#[from]`. `DaemonError` in phase G is an enum from the first line — not a second string registry.
- **Golden code test, per crate**: a test enumerates every error variant, renders `code().qualified()`, and compares the sorted list against a committed `tests/golden/error-codes.txt` extracted from the TS source. Adding a variant without updating the golden file fails the build. This is what makes "exact `ZVEC_GREP.ENGINE.*` strings" verifiable instead of aspirational.

### M2. Newtypes, never aliases, for domain concepts

`pub type X = Y` is permitted only for `Result` sugar (`EngineResult<T>`) and closure-shape abbreviations that are *not* domain concepts. Everything with a semantic invariant is a newtype with a **private field** and a validating constructor.

Fix list from the current tree:

| Today | Required |
| --- | --- |
| `pub type SessionResult<T> = EngineResult<T>` (`service/types.rs:330`) | delete — an alias of an alias |
| three unrelated progress callbacks: `models::ProgressSink`, `service::types::ProgressSink<'a>`, `pipeline::indexing::ProgressCallback` | one `ProgressSink` newtype per event type, distinctly named (`ModelLoadSink`, `IndexProgressSink`); no two `ProgressSink`s in one crate |
| `pub struct UnixMillis(pub i64)` (`types/mod.rs:41`) | private field + `UnixMillis::from_millis` / `now()` |
| `pub struct FileFormat(pub String)` (`types/file.rs:27`) | private field + validating `parse`; public tuple fields make the newtype decorative |
| `pub struct CancelFlag(pub Arc<AtomicBool>)` (`pipeline/indexing/scanner.rs:117`) | private field + `is_cancelled()` / `cancel()`; see M6 |
| `FileId::from_raw(String)` unvalidated while `EntityId::parse` validates (`ids.rs`) | one convention: `parse` validates, `from_raw` is `pub(crate)` and documented as trusted-storage-only |
| `create_embedding_model(reference: &str, ..)` while `ModelReference(String)` already exists (`models/catalog.rs:24`) | take `&ModelReference`; the newtype exists precisely to stop `&str` from flowing in |

Also: raw `usize`/`u64` for bounded quantities become newtypes where a mix-up is silent — `Dimension`, `BatchSize`, `TokenBudget`, `Generation`, `RootKey`, `WorkspaceFingerprint`, `SessionId`, `RequestId`, `TraceParent`. Phase G/H invent a lot of these; they are newtypes on first use, not `String`/`usize` params.

**The freeze is lifted for internal signatures.** The previous plan froze `create_embedding_model`, the DTOs, and storage traits. That freeze is only load-bearing for *wire* formats. Both downstream crates are stubs today: nothing outside `zg-core` consumes any of these signatures, so this is the cheapest moment in the project's life to tighten them, and the most expensive moment to defer. Frozen: on-disk formats, HTTP routes, MCP tool names and bounds, CLI flags, error code strings. Not frozen: any Rust signature.

### M3. Make invalid states unrepresentable

- `EmbeddingModelOwnership::{Owned, Borrowed}` (`service/types.rs:23`) is deleted. It is a GC-language artifact; in Rust ownership *is* the type system, and an `Owned`/`Borrowed` discriminant sitting next to an `Arc` is a bug waiting for a `match`.
- `ZvecGrepInfoResult` currently carries six independent `Option` fields whose valid combinations are a subset of 64. Model it as an enum (`IndexState::{ NotIndexed { suggestion }, Disabled { policy }, Indexed { embedding, index, status } }`) and keep the exact TS JSON shape via a `#[serde(into = ..., from = ...)]` wire adapter.
- `RootPathSpec::Full(RootPath)` triggers `clippy::large_enum_variant` (224-byte spread) — `Box` it.
- Domain types and wire types are separate. Anything with `#[serde(rename_all = "camelCase")]` lives in a `wire` module at the crate boundary and converts via `TryFrom`. `service/types.rs`'s own header notes the TS wire format is "camelCase except where the daemon status DTO explicitly uses snake_case" — that inconsistency is a wire fact and must not leak into domain structs.

### M4. Callbacks that cross the async boundary are owned and `Send + Sync`

`service/types.rs:17-19` defines `AbortCheck<'a> = &'a dyn Fn() -> bool` and `ProgressSink<'a> = &'a dyn Fn(&IndexProgress)`, and `ZvecGrepIndexOptions<'a>` holds both — so the options struct is **not `Send`** and cannot be moved into `tokio::task::spawn_blocking`. The old plan handled the whole sync/async seam in nine words ("async wrapping happens in step G via `spawn_blocking`") while simultaneously freezing these types.

Rule: any callback reachable from a type that crosses the runtime boundary is `Arc<dyn Fn(..) + Send + Sync + 'static>`. Borrowed `&dyn Fn` is allowed only in leaf helpers that never appear in a struct field. This changes in **phase 0**, while nothing depends on it.

### M5. Contracts frozen, internals idiomatic

"Each file mirrors its TS namesake with snake_case symbols" is a transliteration mandate and is hereby scoped down. It applies to observable behavior only. It does **not** license importing TS's internal structure, because the idioms that make TS code work are the ones Rust punishes: promise chaining → generation counters, shared mutable `Map`s → lock soup, `closePromise?: Promise<void>` → nothing at all.

Every module that intentionally diverges from its TS namesake records one line in `docs/ts-divergence.md`: TS symbol, Rust shape, reason. That file is the review artifact. "Mirrors its TS namesake" is otherwise unfalsifiable as an acceptance criterion.

### M6. Cancellation and concurrency are typed, not ad hoc

`tokio::task::spawn_blocking` tasks **cannot be aborted**. Phase G's scheduler has a `cancelled` state, so cancellation must be cooperative and typed end to end: `tokio_util::sync::CancellationToken` at the async edge → `CancelFlag` (already exists, `IndexOptions::cancel`) inside the blocking body, adapted once in a single documented place. Shutdown awaits in-flight blocking work; it does not drop the handle and hope.

Embedding is CPU-bound; unbounded `spawn_blocking` oversubscribes a 512-thread pool. All embed work passes through one bounded gate (`Semaphore` sized to `std::thread::available_parallelism`, or rayon) owned by the model pool.

### M7. Heavy backends are cargo features, off by default

`ort` (build-time binary download) and `llama-cpp-2` (C++ toolchain, libclang/bindgen) must not become unconditional dependencies of `zg-core`, which everything depends on — that makes `cargo install zg` require a C++ toolchain and a network fetch, and lets `cargo test -p zg-core` fail for reasons unrelated to the change under test.

```toml
[features]
default = []
onnx  = ["dep:ort"]
llama = ["dep:llama-cpp-2"]
```

Factory arms are `#[cfg(feature)]`-gated and return `ModelError::BackendUnavailable` when compiled out. The old plan already invented that error for the *build-failure contingency* — features make it a deliberate design instead of a discovery. Both deps are pinned to exact minor versions; `ort`'s 1.x/2.x-rc API split is load-bearing (see phase C).

### M8. Lints are configuration, not a command-line flag

`[workspace.lints]` in the root `Cargo.toml`, inherited by all three crates:

```toml
[workspace.lints.rust]
unsafe_code = "forbid"
missing_docs = "warn"
unused_qualifications = "warn"
[workspace.lints.clippy]
all = { level = "deny", priority = -1 }
todo = "deny"
dbg_macro = "deny"
unwrap_used = "deny"          # allow(unwrap_used) permitted in #[cfg(test)] only
expect_used = "warn"
```

A `-D warnings` invocation nobody remembers to type is not a gate.

## Execution order

```
0 → A → B → E → F → G → H → I → (C ‖ D)
```

Two changes from the previous revision, both about keeping risk off the critical path:

- **Phase 0 is new.** The tree does not currently pass the gate every other phase is measured against, and three pieces of infrastructure the later phases assume (a shared test stub, `Send` callbacks, typed errors) do not exist.
- **C and D move to the end and run in parallel.** They were at positions 3–4. They are the only phases with toolchain risk (bindgen, libclang, prebuilt-binary download, C++ compiler), and **nothing in B–I depends on them**: phase A alone unblocks end-to-end via the pure-Rust model2vec backend, as the plan itself observes. Running them last means the entire tree is provably green before anyone touches `bindgen`, and their failure degrades to "two catalog entries unavailable" instead of "plan stalled".

---

### Phase 0. Make the gate real (new; blocks everything)

1. Clear the 15 baseline findings: unused imports (`service/types.rs:12` `FileScanDiagnostics`, `pipeline/search/mod.rs:26` `has_path_glob` — both are seams later phases consume, so wire or delete deliberately), never-read `models/download.rs:41` field, `large_enum_variant` on `RootPathSpec::Full` (M3), `derivable_impls` on `impl Default for EmbeddingPurpose` (`models/embeddings.rs:71`), `manual_clamp` in `lexical/enrichment.rs:309,311`, `needless_borrow` in `utils/glob.rs:331`, and the rest.
2. Add `[workspace.lints]` per M8 and `lints.workspace = true` to all three crate manifests.
3. Apply M1 to `error.rs` + `models/`: `EngineErrorCode` becomes `&'static str`-backed, `ModelError`/`StorageError`/`IndexingError` enums land, `codes::extractor` dies, the golden error-code test and the `grep` CI gate land.
4. Apply M2/M4 to `service/types.rs` and the three progress callbacks. **Do this before phase B**, not after — B, G, H and I all consume these types.
5. **Ship the shared test stub.** There is no reusable fake today: the only `impl EmbeddingModel` stub is a `struct Failing` declared inside a `#[cfg(test)] mod` in `pipeline/indexing/mod.rs:2294`. `cfg(test)` items are invisible to other crates' tests entirely — `zg-core` is compiled *as a dependency* for `zg-server`'s test target, with `cfg(test)` unset. So phases G and H's "backend index→status→search against a tempfile workspace with stub model" **cannot compile** as previously specified. Add:

   ```toml
   [features]
   test-support = []
   ```

   ```rust
   #[cfg(feature = "test-support")]
   pub mod test_support {
       pub struct StubEmbeddingModel { /* deterministic hash-of-input vectors */ }
       pub fn fixture_workspace() -> tempfile::TempDir { /* .. */ }
   }
   ```

   `zg-server` and `zg` take `zg-core = { workspace = true, features = ["test-support"] }` in dev-dependencies. TS solves this with `test/helpers/fake-embedding.mjs`; Rust needs a crate-level answer and it is a phase-0 deliverable.

Exit: `cargo test --workspace` green, `cargo clippy --workspace --all-targets` clean with zero `#[allow]` added outside tests, golden error-code test present, `StubEmbeddingModel` reachable from `zg-server`'s test target.

### Phase A. Wire factory dispatch (unblocks all e2e)

In `models/factory.rs`, `create_embedding_model` currently returns `EMBEDDING_MODEL_NOT_IMPLEMENTED` on every arm by design (mirrors TS `unsupportedCatalogEntry`). Replace the model2vec and both Qwen arms with the existing constructors (`Model2VecEmbeddingModel::from_plan` at `backends/model2vec.rs:71`, `QwenTextEmbeddingModel::from_plan` at `backends/qwen.rs:267`, `Qwen3VlEmbeddingModel::from_plan` at `backends/qwen.rs:443`), leaving `plan_embedding_model`'s validation logic intact. `transformers-js` and `llama-cpp` arms return `ModelError::BackendUnavailable` gated on the M7 features until C/D land; unknown references keep `EMBEDDING_CATALOG_MODEL_NOT_FOUND`.

Signature becomes `pub fn create_embedding_model(reference: &ModelReference, options: &CreateEmbeddingModelOptions) -> Result<Arc<dyn EmbeddingModel>, ModelError>` per M1/M2. No existing test asserts the `NOT_IMPLEMENTED` behavior (verified — only doc comments reference it), so nothing regresses.

Tests: extend `factory.rs`'s existing four tests — model2vec and each Qwen kind resolve to a working model with `info().dimension` matching the catalog (verified against `models/catalog.rs`: `local/potion-retrieval-32m` 512, `qwen/text-embedding-v4` 1024, `qwen/qwen3.7-text-embedding` 1024, `qwen/qwen3-vl-embedding` 2560); unknown reference errors with `MODEL_NOT_FOUND`; feature-gated arms error with `BackendUnavailable` when compiled out.

### Phase B. `ZvecGrepService` facade in `zg-core`

Create `crates/zg-core/src/service/service.rs` (`service/mod.rs` today re-exports only `root/types/workspace_index`) implementing `create_zvec_grep(opts: CreateZvecGrepOptions) -> ZvecGrepService`, mirroring `../zvec-grep/src/engine/service/zvec-grep.ts` and reusing the phase-0-corrected DTOs in `service/types.rs`. Methods delegate to the existing `WorkspaceIndex` (`service/workspace_index.rs`), `pipeline::indexing`, `pipeline::search`: `open_workspace`, `ensure_index` (progress via the M4 owned `IndexProgressSink`), `drop_index`, `search`, `rg_search` (delegates to `lexical/mod.rs` in-process; never a subprocess), `index_status`, `workspace_info`.

Model identity per workspace resolves via `models/resolution.rs`. `EmbeddingModel::embed` is sync, so the facade stays sync and takes no tokio dependency; the async wrapping in phase G is specified there, per M4/M6, rather than assumed.

**Read-session ownership.** The previous plan gave the facade an idle-TTL read-session cache *and* gave phase G a `workspace_read_session_cache.rs` doing the same thing. Pick one owner: the **daemon**. The facade exposes explicit `open_read_session`/`close_read_session` returning an RAII `ReadSession` guard and holds no timers; TTL eviction is the daemon's concern (it is the only layer with a runtime). A sync facade with an idle TTL would otherwise need either a background thread or lazy expiry-on-access — an unstated design decision in the old plan.

Tests: open / index-status / search-plan round trip on a `tempfile` fixture workspace using phase 0's `StubEmbeddingModel`, no network.

### Phase E. TS index compatibility — and its two-way hazard

`storage/zvec/store.rs::FileMetaStore` persists file metadata as `files.json` while TS writes a second zvec collection (`files.zvec`); `storage/layout.rs:16-18` knows both names and `has_workspace_index_storage` accepts either. The entity collection `index.zvec` is **shared and format-compatible** via `zvec-rust 0.7`.

That last fact makes this a two-way problem, which the previous plan treated as one-way. Importing TS→Rust while "deleting nothing" leaves a directory holding a fresh `files.json`, an `index.zvec` that Rust has mutated, and a **stale `files.zvec`**. A user with both CLIs installed then has TS diffing stale metadata against entities Rust rewrote — two tools thrashing one index, each convinced it is current, with no error surfaced. Silent divergence of user data is worse than a reindex.

Required, in order of preference — **decide before writing code**:

1. **Version the storage** so a Rust-written index is unmistakable to TS (bump the index version in the manifest / use a distinct storage subdir). TS treats it as foreign and reindexes; Rust imports TS indexes read-only. Cleanest, no shared mutable state.
2. Write **both** stores (`files.json` and `files.zvec`) on every index run, keeping bidirectional compatibility.
3. Delete `files.zvec` after a verified successful import, accepting explicit one-way migration.

Whichever is chosen, `docs/ts-divergence.md` records it and a test asserts it. Best-effort import itself: on open, if `files.json` is absent and `files.zvec` exists, read via `zvec-rust` into `FileInfo` (`types/file.rs`); if the schema will not open, delete nothing, log at `warn`, and fall back to full reindex.

**Test fixture must be real.** The old plan's test synthesized a "TS-shaped" `files.zvec` *with `zvec-rust` from Rust* — that validates Rust's guess at the TS schema, not the schema, and passes even if the guess is wrong. Run the TS indexer once against a committed fixture workspace, commit the resulting `files.zvec` + `index.zvec` under `crates/zg-core/tests/golden/ts-index/`, and assert the import yields exact `FileInfo`s. Add a corrupt-collection case asserting the reindex path, never an error.

### Phase F. Authorization (in `zg-core`) + observability (in `zg-server`)

**Authorization moves to `zg-core`**, contrary to the previous plan. That plan put `auth/` in `zg-server` while also having `zg --mode direct` call `ZvecGrepService` in-process — so remote Qwen embedding in direct mode either bypasses the permit guard entirely or forces the CLI to link the whole axum/rmcp daemon stack to run a local search (which is why `zg` currently dev-depends on `zg-server`). TS keeps `src/authorization/` as a *peer* of `daemon/`, not inside it; the crate split turns "move it into the server" into a behavior difference, not a refactor.

Split by concern:

- `zg-core::authorization/` — the policy: grant store keyed by `WorkspaceFingerprint` (newtype, M2), `plan_remote_index_authorization` / `plan_remote_search_authorization`, and the `with_remote_embedding_operation_permit` guard wrapping every remote-embedding call site. Mirrors `../zvec-grep/src/authorization/` (`manager/store/operation/planner/target/prompt/types.rs` + re-exports). `AuthError` is an enum (M1).
- `zg-server::trace.rs` — W3C `traceparent`/`tracestate`/baggage over MCP request meta via `tracing` spans, mirroring `../zvec-grep/src/observability/trace-context.ts`. `TraceParent`/`RequestId` are newtypes.
- `zg-server::logger.rs` — request-id JSONL via `tracing-subscriber/json`, porting `daemon/logger.ts`.

Prompt text stays byte-identical to TS (it is user-facing contract). Tests: grant/status/revoke round trip on a `tempfile` store; trace-context inject/extract round trip; a test asserting a remote-embedding call **without** a permit fails closed.

### Phase G. Daemon in `zg-server` — actors, not replicated fields

This is the phase the previous plan under-specified most seriously, and it is the architectural core of the port.

That plan said to give `DaemonBackend` "the exact fields of `../zvec-grep/src/daemon/backend.ts:106-123` — modelPool, runtimeManager, scheduler, statusCache, watchers, coordinators, droppingRoots, authorizationManager". Two problems. First, the inventory is incomplete: the class (`backend.ts:106`) carries thirteen fields, and the enumeration omits five — `startedAt`, `lastScanDiagnostics`, `workspaceRuntimeCache`, `shuttingDown` (line 124), and `closePromise` (line 125, past the end of the cited range). One of them, `closePromise?: Promise<void>`, has no Rust analogue at all. Second and worse: in TS those are plain `Map`s mutated freely across `await` points, safe **only** because of the single-threaded event loop. Replicating them field-for-field in Rust produces either eight independent `Arc<Mutex<HashMap>>` (lock-ordering deadlocks the first time `index_coordinator` and `watch_manager` both touch runtime + status) or one `Mutex<State>` that serializes the entire daemon.

**Required design: one actor task per root.** Each root is owned by a single task holding `RootRuntime`, its coordinator, and its watcher as plain `&mut` state, driven by an `mpsc::Receiver<RootCommand>`. `DaemonBackend` holds only `Arc<RwLock<HashMap<RootKey, RootHandle>>>` where `RootHandle { tx: mpsc::Sender<RootCommand>, .. }`. Consequences, all improvements:

- "Per-root generation-chained indexing" (a promise-chain idiom) becomes sequential message processing. `Generation` survives as a newtype for staleness checks against target revisions, not as a concurrency mechanism.
- `droppingRoots: Set` and `shuttingDown: bool` become states in a `RootState` / `DaemonState` enum — invalid combinations stop being representable (M3).
- `closePromise` becomes a `CancellationToken` plus awaiting the actor `JoinHandle`s (M6). Shutdown is deterministic.
- `statusCache` / `lastScanDiagnostics` / `workspaceRuntimeCache` move *inside* the owning actor; cross-root reads go through a command, so there is no shared-map staleness.

Build order, each module mirroring its TS namesake in `../zvec-grep/src/daemon/` with snake_case symbols, enum errors (M1), and newtype keys (M2): `errors.rs` (`DaemonError` enum + golden code test) → `change_set.rs` (created/changed/deleted + rescan-dir/deleted-prefix accumulation) → `job_scheduler.rs` (queued/running/succeeded/failed/cancelled as an enum state machine, retry, per-root dedupe, cooperative cancel per M6) → `model_pool.rs` (LRU model cache with RAII leases and idle TTL; loads via `zg-core::models::factory::create_embedding_model` inside `spawn_blocking`, and owns the single bounded embed semaphore from M6) → `root_runtime.rs` + `runtime_manager.rs` (the actor and its registry) → `index_coordinator.rs` (target-revision reconciliation) → `watch_manager.rs` (`notify`-based recursive watch coalescing to `ChangeSet`; adds the `notify` dep) → `workspace_read_session_cache.rs` (idle-TTL read sessions — sole owner per phase B) → `backend.rs` (command surface: search / index / index-drop / rg / index-status / server-status) → `http_server.rs` (axum 0.8: `GET /healthz`, `POST /control/shutdown`, `POST /mcp`, `POST /mcp/admin`; loopback-host guard, bearer token, 1 MB body cap) → `server_controller.rs` (spawn/kill, `instance.lock`, readiness probe) → `config.rs` (`ServerConfig`, default listen `127.0.0.1:7999`, token paths).

TS `root-lease.ts` maps onto the existing `utils/lease.rs` single-writer lease; TS `runtime.ts` maps onto the existing `config::EmbeddingRuntimeConfig` / `ResolvedEmbeddingRuntimeConfig` (`config/mod.rs:86,99`) — neither needs a new module, and both were unaccounted for in the previous plan's file list.

Tests (using phase 0's `StubEmbeddingModel` — impossible without it): scheduler dedupe/retry/cancel, change-set accumulation, coordinator target-revision proof, backend index→status→search against a `tempfile` workspace, HTTP healthz, shutdown-auth rejection, non-loopback rejection, body-cap rejection, and a shutdown-during-index test proving in-flight blocking work is awaited rather than orphaned.

### Phase H. MCP endpoint in `zg-server`

New `crates/zg-server/src/mcp/` mirroring all 10 modules of `../zvec-grep/src/mcp/` over `rmcp` 0.6: `schemas.rs`, `toolset.rs`, `tools.rs`, `input_normalization.rs`, `result_format.rs`, `request_state.rs`, `request_metadata.rs`, `progress_heartbeat.rs`, `stdio_bridge.rs`, `http_transport.rs`.

Frozen contract: tool names `zvec_grep_search`, `zvec_grep_index`, `zvec_grep_index_drop`, `zvec_grep_rg`, `zvec_grep_index_status`, `zvec_grep_server_status`; bounds 32 groups / 4000 chars / 128 path filters / limit 50; toolset `agent|full` resolved flag → `ZVEC_GREP_MCP_TOOLSET` → `agent` default; StreamableHTTP modern + legacy session LRU (256 entries / 30 min idle).

Per M2, the numeric bounds are `const` newtype values validated in one place, and inputs are parsed into validated newtypes at the boundary (`GroupCount`, `QueryLength`, `PathFilterCount`) so a handler cannot receive an out-of-range value. `tools.rs` handlers delegate to `DaemonBackend` commands and reimplement no logic. `McpError` is an enum mapping to rmcp error payloads (M1).

Tests: schema-bound rejection (over-limit inputs error before touching the backend), toolset gating (agent set hides index/rg/status), stdio smoke against an in-process backend, and — from the TS suite the previous plan skipped — `mcp-modern-http` and `mcp-legacy-http` session-lifecycle equivalents.

### Phase I. `zg` CLI + client

Replace `crates/zg/src/main.rs` with a clap-derive tree mirroring `../zvec-grep/src/cli/args.ts` command-for-command: `query` (incl. `--rg`, `--hybrid-queries`, path filters, `--refresh`, `--mode direct|server|auto`), `index` (`--drop` + `--yes`), `status` (`--check-ready`), `config model|provider set`, `auth grant|status|revoke`, `server on|off|status|run|--stdio` (`--listen`, `--token-file`, `--mcp-toolset`), `install/uninstall`, `help`, `version`. Wire `clap_complete` (already a declared, unused dep) into a `completions` subcommand.

New `crates/zg/src/client.rs` mirroring `../zvec-grep/src/client/`: `mode_router.rs` (`route_by_mode` over a `ClientMode` enum — the type already exists at `config/mod.rs:110`), `daemon_client.rs` (StreamableHTTP + progress heartbeat + token auth), `search_policy.rs`. Direct mode calls `ZvecGrepService` in-process; server mode goes through the daemon client. Because authorization now lives in `zg-core` (phase F), direct mode enforces the same permit guard as the daemon — and `zg`'s dependency on `zg-server` should be re-examined and dropped if `server run` can spawn via `zg-server` as a binary dependency only.

Port `cli/format/*` to `crates/zg/src/format.rs` (hits/ranges/progress/status renderers) and `cli/install.ts` behavior (IDE MCP config read-modify-write behind a `--yes` gate; never overwrite without `--yes` or an explicit existing-`zg` block merge). `managed-rg.ts` maps to an error directing to `zvec_grep_rg` when flags are incompatible, matching `server-search.ts` text.

Tests: per-command arg snapshots, format goldens, install merge against a `tempfile` HOME, plus the `mode-router`, `daemon-client`, `server-search`, `rg-cli`, `config-cli`, and `server-cli` behaviors from the TS suite.

### Phases C and D (parallel, last). Feature-gated local backends

Both are gated per M7, both return `ModelError::BackendUnavailable` when compiled out, and neither is on the critical path.

**C — ONNX (`transformers-js` entries).** New `models/backends/onnx.rs` for `local/bge-small-en-v1.5` (pooling `cls`, dim 384, dtype `q4`, `maxInputTokens` 512, query prefix `Represent this sentence for searching relevant passages: `) and `local/all-minilm-l6-v2` (pooling `mean`, dim 384, `maxInputTokens` 256), per `../zvec-grep/src/engine/models/backends/transformers-js.ts` and `catalog.ts:73-104` (values verified against `models/catalog.rs:334-363`).

**D — llama-cpp GGUF.** New `models/backends/llama_cpp.rs` for `local/embeddinggemma-300m` (dim 768, ctx 2048, batch 16, format `embeddinggemma`) and `local/qwen3-embedding-0.6b` (dim 1024, ctx 8192, batch 8, format `qwen3`), per `.../llama-cpp.ts` and `catalog.ts:6-31`.

**The shared blocker both phases must solve first — previously unmentioned.** `trait EmbeddingModel: Send + Sync` with `fn embed(&self, ..)` (`models/mod.rs`), and the factory hands back `Arc<dyn EmbeddingModel>` that phase G's pool shares across tasks. But `llama-cpp-2`'s `LlamaContext` borrows its `LlamaModel`, is not `Sync`, and decodes through `&mut self`; `ort`'s `Session::run` receiver differs across the 1.x/2.x-rc line. Both backends therefore need interior mutability, and the choice is load-bearing:

- `Mutex<Session>` / `Mutex<Context>` serializes every embed call process-wide, defeating `maxBatchSize` and the model pool.
- **Required instead:** a context-per-lease pool — the `Arc<dyn EmbeddingModel>` owns the `'static` model weights, and each embed acquires a context from a bounded pool (which is also M6's gate). For llama-cpp this additionally resolves the self-referential-lifetime problem of holding a `LlamaContext<'a>` inside a `'static` trait object: contexts are created per lease from the owned model, never stored.

Write a one-page design note in `docs/ts-divergence.md` for this before either implementation. This is the most likely place the port actually stalls, and the previous plan gave each phase a single sentence.

Tokenizers load through `models/download.rs`'s cache at the catalog's pinned `repo`/`revision` — **not** via the `tokenizers` crate's `http` feature directly, or offline runs break. Truncate at `maxInputTokens`, apply catalog pooling, L2-normalize when `normalize: true`, and note that "batch at `maxBatchSize`" is the *caller's* job: `validate_contents` (`models/embeddings.rs`) **errors** when `inputs.len() > max_batch_size`, so a backend cannot both chunk at 4 and reject 5. Chunking stays in `pipeline::indexing`; document it.

**Vector-parity gate (mandatory, blocks phase sign-off).** The old plan verified pooling/prefix logic against fakes plus `#[ignore]`d download tests that its own verification section never ran — so the *numeric output* of these backends was never compared to TS. Combined with phase E reading TS-written `index.zvec`, that is a silent-corruption path: a vector that differs by a pooling or prefix detail still has the right dimension and is finite, so `validate_result` passes, the index looks healthy, and retrieval quality quietly degrades with nothing to alert on.

Required: for each of the four local models, commit `crates/zg-core/tests/golden/vectors/<model>.json` — N inputs (short, long/truncated, unicode, query-purpose, document-purpose) with vectors produced by the TS backend — and assert cosine similarity > 0.9999 in a **non-ignored** test that runs whenever the model is present in the cache and skips with a printed reason otherwise. CI caches the model directory so the test actually runs.

---

## Critical files & anchors

- `crates/zg-core/src/models/factory.rs:146-167` — `create_embedding_model` stub body; phases A/C/D.
- `crates/zg-core/src/error.rs:11-31,90-115` — `EngineErrorCode(String)` + `codes::` string builders; the M1 target.
- `crates/zg-core/src/service/types.rs:17-19,23,55,63-79,330` — borrowed callbacks (M4), `EmbeddingModelOwnership` (M3), `large_enum_variant` (M3), `Option`-cluster DTO (M3), dead alias (M2).
- `crates/zg-core/src/models/mod.rs` — `EmbeddingModel` trait (`&self` + `Send + Sync`); constrains phases C/D.
- `crates/zg-core/src/storage/layout.rs:5-18,45` — `files.json` / `files.zvec` / `index.zvec` divergence; phase E.
- `crates/zg-core/src/pipeline/indexing/mod.rs:2294` — the only existing `EmbeddingModel` fake, and why phase 0 must extract it.
- `../zvec-grep/src/daemon/backend.ts:106-125` — the *full* `DaemonBackend` field set (13 fields, not the 8 previously listed); phase G, as an inventory of state to place, not a shape to copy.
- `../zvec-grep/src/mcp/tools.ts` + `schemas.ts` — MCP names, gating, bounds; phase H.
- `../zvec-grep/src/cli/args.ts` (~1520 lines) + `commands.ts` (~978 lines) — CLI surface; phase I.
- `../zvec-grep/test/` — 60 suites / ~21.6 kLOC; the coverage target, see below.

## Verification

Per phase: `cargo test -p <crate>` plus `cargo clippy --workspace --all-targets` (clean by configuration per M8, no flags). The tree is green after every phase.

Standing gates, all enforced in CI (which this plan adds — `.github/workflows/ci.yml`, mirroring the TS repo's):

1. `cargo test --workspace --all-features` and `--no-default-features`.
2. `cargo clippy --workspace --all-targets --all-features`.
3. `grep -rn 'EngineErrorCode::new(&format!' crates/` returns nothing (M1).
4. Golden error-code lists match, per crate (M1).
5. `cargo doc --workspace --no-deps` with no warnings (M8 `missing_docs`).
6. `cargo deny check` — licenses and advisories, mandatory once a vendored-C++ dep exists.
7. MSRV: `rust-version = "1.85"` is verified against `ort` and `llama-cpp-2` before phases C/D commit to versions; if either requires more, bump deliberately and note it.

New-behavior proof per phase: **0** — clippy-clean tree, `StubEmbeddingModel` used from `zg-server`'s test target; **A** — `create_embedding_model("local/potion-retrieval-32m")` yields a dim-512 model; **B** — facade round trip on a stub; **E** — import from a *real* committed TS-written index, plus the chosen compatibility stance asserted; **F** — grant round trip + remote embedding fails closed without a permit; **G** — backend index→status→search, HTTP healthz / auth / loopback / body-cap rejections, shutdown-awaits-in-flight-work; **H** — bound rejection + toolset gating + both HTTP session lifecycles; **I** — arg snapshots, format goldens, install merge; **C/D** — vector parity > 0.9999 against committed TS goldens.

**Coverage target, stated honestly.** The previous plan closed by claiming the TS suite was "covered by ported tests: factory, job-scheduler, daemon-backend, mcp-contract, install" — 5 of 60 suites, presented as parity. The real P0 list, which this plan's phases do cover: `factory`, `job-scheduler`, `daemon-backend`, `mcp-contract`, `mcp-modern-http`, `mcp-legacy-http`, `install`, `authorization`, `authorization-prompt`, `change-set`, `index-coordinator`, `watch-manager`, `root-lease`, `server-http`, `server-controller`, `mode-router`, `daemon-client`, `server-search`, `trace-context`, `daemon-logger`, `config`, `config-cli`, `server-cli`, `embedding-runtime`, `service-model-cache`, `service-config-reload`, `daemon-cache`, `path-indexing`, `zvec-storage`, `e2e/cli`. Deferred with reasons recorded: `package`, `smoke/local-model-package` (npm-packaging specific), `model2vec-worker-pool` (no worker threads in the Rust design). Anything not in either list is an accepted gap, not silence.

Final e2e (release binary, no network beyond model download): `zg index <fixture-root> --mode direct` with a local catalog model, `zg query "<term>" --mode direct` returns the fixture hit; `zg server run` + `curl GET /healthz` → 200; `zg status` output matches `zvec_grep_index_status`.

**Performance parity is a gate, not an aspiration.** A Rust port whose justification is speed with no benchmark is unfalsifiable. `../zvec-grep/benchmarks/swe-qa-bench/` already exists with committed datasets: run it against both implementations once phase I lands and record index time, query p50/p95, peak RSS, and retrieval quality (the port must not lose recall). Ship the numbers in the README.

## Assumptions & contingencies

- Phases C/D are feature-gated (M7), so a failing `ort` or `llama-cpp-2` build is not a stall: `default = []` keeps the tree green, `BackendUnavailable` is the typed answer, and the two affected catalog entries are documented as requiring `--features onnx` / `--features llama`. Record any per-target build failure in `docs/ts-divergence.md`, not as a silently `#[ignore]`d test.
- If `zvec-rust` cannot open a TS-written `files.zvec` at all, phase E's importer is dropped and every TS index is treated as reindex-needed (the reindex path is asserted either way). Never migrate user data through a `files.json`-as-canonical rewrite of a directory TS may still own.
- If the committed TS golden vectors cannot be produced (TS backend unrunnable on the build host), phases C/D do **not** sign off on unit tests alone — generate goldens once on any machine that can run TS and commit them. This gate is not optional; it is the only thing standing between a subtle pooling bug and a silently degraded index.
- MCP/daemon numeric contracts (ports, bounds, TTLs, tool names) are frozen from TS; conflicts between the rmcp 0.6 API and TS transport behavior resolve in favor of observable behavior via an adapter, never by renaming. Internal Rust signatures are explicitly **not** frozen (M2).
- The actor design in phase G is a deliberate divergence from `backend.ts`'s structure and will make line-by-line diffing against TS harder. That cost is accepted: the alternative is eight mutexes around a shared object graph that TS only gets away with because it never runs two of these callbacks at once.

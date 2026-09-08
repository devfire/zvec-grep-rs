# Rust Hardening Action Plan

Source: defensive skill (`rust-defensive-programming`, 11 pts) + advanced skill (`advanced-rust-patterns`, 10 pts) + error/API + neckbeard audits, 2026-09-08.
Rule: no behavior change except where noted (P0 error propagation). Each phase commits separately.

## Phase 0 — Lint firewall (highest leverage, unlocks the rest)

- [ ] `Cargo.toml [workspace.lints.clippy]`: add `indexing_slicing`, `fallible_impl_from`, `wildcard_enum_match_arm`, `unneeded_field_pattern`, `fn_params_excessive_bools`, `must_use_candidate` as `deny`.
- [ ] Same file: `expect_used` `warn` → `deny` (keep `cfg(test)` allows; `crates/zg/src/main.rs:9` already shows the pattern).
- [ ] Add `panic = "deny"`; audit the 5 existing sites (Phase 2) with targeted `#[allow]` + justification.
- [ ] Add `missing_errors_doc = "warn"` (→ deny later), `missing_panics_doc = "deny"`, `result_large_err = "warn"`, `print_stdout/print_stderr` warn (allow in `zg` bin).
- [ ] Delete dead `anyhow = "1"` from `[workspace.dependencies]` (`Cargo.toml:16`) — zero crates depend on it.
- [ ] Verify: `cargo clippy --workspace --all-targets` clean; `cargo test --workspace` green. Commit.

## Phase 1 — Stop silent error loss (P0, behavior change, review carefully)

- [ ] `zg-core/src/pipeline/search/mod.rs:536-537,545-546,906-907,916-917,933-943`: `search_fts/vector(...).unwrap_or_default()` → propagate `?` on primary route; supplemental routes may degrade only with a diagnostic counter. Corrupt shard must not read as "no results".
- [ ] `zg-core/src/lexical/enrichment.rs:162,169,197`: `.ok()?` in `Option` helpers → return `Result` internally or record diagnostic (None must mean "absent", not "failed").
- [ ] `zg-core/src/models/backends/model2vec.rs:361-366`: tokenizer `.ok()?` → propagate/log; `:355-358` `.lock().ok()` → propagate.
- [ ] `zg-core/src/pipeline/indexing/mod.rs:1079-1081` `lock_stats()` poisoned-mutex → typed error, not zero stats.
- [ ] `zg-core/src/service/facade.rs:842-843` `let _ = handle.join()` → `if let Err` + trace log.
- [ ] `zg-server/src/model_pool.rs:97-104` throwaway `String::new()` sentinels for `.code()` → `const fn code_for` or free consts.
- [ ] `zg-server/src/mcp/error.rs:55-58` `into_error_data()`: put wire `code` in `data` (`json!({"code": ...})`), not `None`.
- [ ] Verify: new regression tests — recall failure surfaces error (not empty hits); poisoned-mutex test; `cargo test -p zg-core -p zg-server` green. Commit.

## Phase 2 — Panics → typed errors

- [ ] `zg-core/src/error.rs:249,253,257` `panic!` in `LazyLock` redaction init → `LazyLock<Result<Regex>>` with skip-pass fallback, or documented `#[allow]` + `missing_panics_doc`.
- [ ] `zg-core/src/pipeline/indexing/mod.rs:290,350` ×2 `unreachable!` → `EngineError::new(INDEXING.SCHEDULER_FAILED / WORKSPACE_FAILED, …)`.
- [ ] `zg-server/src/read_session_cache.rs:131` `unreachable!` → `SessionError::Closed`.
- [ ] `zg-core/src/pipeline/indexing/mod.rs:958-965` + `models/backends/model2vec.rs:486-494`: panicking worker payload dropped → capture into `ModelError::WorkerFailed{ detail }` + log.
- [ ] `models/error.rs:468-472` `WorkerFailed{ reference }` gains `detail: String`.
- [ ] Verify: `cargo clippy` with `panic=deny` passes; shutdown/panic-injection tests green. Commit.

## Phase 3 — Error chains + de-stringly payloads (P1)

- [ ] Implement `source()` forwarding on all six: `EngineError` (`zg-core/src/error.rs:84`), `BackendError` (`zg-server/src/backend.rs:176`), `SessionError`, `AcquireError` (`model_pool.rs:131`), `McpError`, `DaemonError`. Either store `Box<dyn Error>` source in `EngineError` or keep `BackendError::Model(ModelError)` un-flattened one layer up.
- [ ] Un-dead the `caused by:` printer (`zg/src/format/error.rs:20-24`) — must print >1 level after fix; add test.
- [ ] `zg/src/error.rs:19-67`: `CliError::Io{message}` → `Io{path: PathBuf, #[source] error: io::Error}`; document "CLI errors are human text + const code" or add structured fields per variant.
- [ ] `zg-server/src/mcp/error.rs:14-38`: `AuthorizationRequired` gets distinct code/payload (not `-32602` conflated with bad-input).
- [ ] `zg-core/src/authorization/error.rs:41-44` `StoreFailed{operation: String}` → `enum StoreOp{Read,Parse,CreateDir,Write,…}`.
- [ ] `zg-server/src/errors.rs:59-63` `IndexFailed{message}` → `{root, detail}` or `#[source]`.
- [ ] `models/error.rs:309-330,468-478` Qwen `#[error("{message}")]` → derive Display from fields; keep `context` as single detail block.
- [ ] `zg-core/src/config/mod.rs:241-244` + inline `LEXICAL.*/INDEXING.*/SEARCH.*` literals → move into `codes!` registry (`error.rs:100+`) or document module as illustrative; check golden registries cover all.
- [ ] `zg/src/error.rs:152-160` `own_code() -> Option` → non-optional so new code-less variant fails to compile (mirror `DaemonError::all_codes()` golden test).
- [ ] `zg-server/src/logger.rs:50-59` `From<u64/usize>` lossy `as i64` → `TryFrom` (sibling `From<i32>` via `i64::from` is the correct template).
- [ ] Verify: `cargo test --workspace` incl. golden `error-codes.txt` / `daemon-error-codes.txt`; `--debug` prints multi-level chain. Commit.

## Phase 4 — Indexing/slicing → slice patterns (defensive #1, 16 sites)

Fix shape: `match x.as_slice() { [a] / [first, ..] / [.., owner, _] }` or `.get()/.first()/split_first()` + `let-else`. Sites:
- [ ] `zg-core/src/authorization/prompt.rs:101` `names[0], names[1]`.
- [ ] `zg-core/src/storage/zvec/codec.rs:487-489,541-544` chunk/quad indexing.
- [ ] `zg-core/src/models/backends/model2vec.rs:318,323,340,345` shape + chunk bytes (use `first_chunk::<N>()` / `try_into` + let-else).
- [ ] `zg-core/src/models/backends/qwen.rs:224-226` `chunk[0]` (+`:383,593` `vectors[index]` → `get_mut`).
- [ ] `zg-core/src/extraction/code/extractor.rs:479-480,487,496-497` `statements[i]` + sub-slice.
- [ ] `zg-core/src/pipeline/indexing/mod.rs:263-265,340,672-674,1145,1583,1588` passes/unit/files.
- [ ] `zg-core/src/extraction/markdown.rs:288,290,449-450` headings/bytes/line slice.
- [ ] `zg-core/src/extraction/code/families/metadata.rs:169,171` `stripped[2..]/[1..]` → `strip_prefix`.
- [ ] `zg-core/src/pipeline/search/mod.rs:655` `parts[len-2]` → `[.., owner, _]`.
- [ ] `zg-core/src/storage/zvec/filter.rs:63` `values[0]` → `let [single]`.
- [ ] `zg-core/src/pipeline/indexing/scanner.rs:1033-1048` `p[0]/p[1..]` → `split_first()`; `zg-core/src/utils/glob.rs:40-43` same shape.
- [ ] Verify: `clippy::indexing_slicing` deny passes; tests green. Commit.

## Phase 5 — Exhaustiveness + field visibility (defensive #3/#5/#6)

- [ ] Wildcards → explicit: `lexical/enrichment.rs:258-259,293-294,307-308` (`Range::File` arms); `pipeline/indexing/mod.rs:979-981` (`UnitOutcome::Embedded(_) => None`).
- [ ] `{..}` → named ignores: `models/embeddings.rs:171`, `types/content.rs:36`, `pipeline/indexing/mod.rs:1360`, `backends/qwen.rs:308`, `stub.rs:91` (template: `job_scheduler.rs:481` `canonical_root: _`).
- [ ] Manual `Debug` impls → full destructure: `extraction/code/adapter.rs:172-178`, `models/embeddings.rs:63-69,81-87`, `service/facade.rs:524-530`.
- [ ] Verify: `wildcard_enum_match_arm` + `unneeded_field_pattern` deny pass. Commit.

## Phase 6 — Construction integrity (defensive #8/#11, advanced #3/#5)

- [ ] `extraction/mod.rs:22-25` `ChunkOptions` + `vector_content.rs:22-25` `ResolvedChunkOptions`: private fields + validating `new()`/builder (`overlap ≤ max`); or `ChunkConfig::Bounded{max,overlap}`.
- [ ] `ids.rs:19-21,41-43` `from_raw` → `pub(crate)` (template: `authorization/types.rs:27,50` `from_hex`); keep validating `parse`.
- [ ] `authorization/planner.rs:29-37` 4-bool `PlanSearchInput` → `Freshness::Wait/ServeStale`, `VectorRoute::On/Off` enums.
- [ ] `authorization/types.rs:121-124` fold `query_text: bool` into disclosure enum pair.
- [ ] `WorkspaceIndex{closed: bool}` / `FileMetaStore{read_only: bool}` → `WorkspaceIndex<Read|Write>` typestates; `supports_images: bool` (`models/mod.rs:53-56`) → computed only.
- [ ] Seal public traits: `EmbeddingModel`, `RankingModel`, `LanguageAdapter`, `WorkspaceIndexStorage`, `ClosableHandle` (`mod private { pub trait Sealed {} }`).
- [ ] 17/20-field `ZvecGrepIndexOptions`/`ZvecGrepContextOptions` + 12-field `SearchPlan` + 5×`Option` resolution → builders with `build() -> Result`.
- [ ] `models/resolution.rs:81-82` test `..Default::default()` → set fields explicitly.
- [ ] Verify: tests green; no new `pub` bypass compiles (`grep from_raw` only in-crate). Commit.

## Phase 7 — Type-driven backfill (advanced #4 + primitives)

- [ ] Add `#[repr(transparent)]` to id/fingerprint newtypes (`FileId`, `EntityId`, `ModelReference`, `FileFormat`, `ApiKey`, `WorkspaceFingerprint`, `TargetFingerprint`).
- [ ] Path trio → `AbsolutePath/RelativePath/DisplayPath` + `StorageDir` newtypes, `serde(transparent)` (wire unchanged): `types/file.rs:75,108-110,135-136`, `service/types.rs:203-205`, `service/root.rs:22-24`, `types/workspace.rs:51`.
- [ ] Identity strings → `WorkspaceId/Name`, `FragmentGroupId` (`types/entity.rs:106`, `storage/mod.rs:67`), `RouteId` (`types/search.rs:136-137`), `ModelProvider/ModelName`, `ContentHash::parse`, `SymbolName/Scope`, `HfRepo/Revision/Uri`, `Option<ApiKey>/Option<Endpoint>` (`embeddings.rs:50-51`).
- [ ] Numerics → `ByteCount`, `ResultLimit(NonZeroUsize)`, `FiniteF64/Score/Embedding`, `IndexVersion` enum, `Duration`, `FragmentIndex(u32)` (reconcile `codec.rs:38 i32` vs `ids.rs:77 usize`).
- [ ] Runtime checks → constructors: `ModelReference::parse()->Result`, `FileFormat::parse` → `Result`, `UnixMillis::from_millis` → `TryFrom`, `NonEmptyTrimmed/Endpoint`, `FilterLiteral` (`filter.rs:74`), `TopK TryFrom` (replaces silent `clamp_topk`).
- [ ] `&mut dyn FnMut` leaf callbacks (`download.rs:201,273`), `&dyn Fn` (`root.rs:97`), `validate_*(&dyn)` (`embeddings.rs:142,195,248`) → `impl FnMut/Fn` or trait default methods.
- [ ] `#[must_use]` on pure pub fns: `authorization/target.rs:18,52,58`, `models/embeddings.rs:91`, `codec.rs:484`, `hash.rs:6,11`, `glob.rs:17,30,76`.
- [ ] Multi-bool fns → enums/param structs: `format/control.rs:4`, `format/context.rs:14`, `format/error.rs:12`.
- [ ] `families/metadata.rs:44-46` → return `Cow<str>` (sole genuine Cow miss; codebase otherwise 0 `Cow` — correctly so).
- [ ] Verify: tests + doc build green. Commit per sub-batch (paths / numerics / ctors).

## Phase 8 — Neckbeard cosmetics (naming, perf nits, docs)

- [ ] Rename 6 `get_` fns: `get_workspace_index_status` (`indexing/mod.rs:163`), `get_embedding_model_catalog_entry` (`catalog.rs:477` + `facade.rs:634` twin), `get_file_by_path/get_entity` (`storage/mod.rs:92,102`, `zvec/mod.rs:330,397`). Exempt: `get_info` (rmcp trait), `get_or_create_signing_key`.
- [ ] Per-byte hex `format!("{b:02x}")` (`store.rs:47`, `target.rs:46`) → shared helper with lookup table / `write!`.
- [ ] Per-node `kind().to_string()` (`js_ts.rs:200`, `go.rs:50,80`) → compare `kind() ==` directly.
- [ ] `prompt.rs:57,61,63,67` nested `format!` → single `write!`.
- [ ] 535-line `from_model_id` (`models/error.rs:37`) → table-driven/`phf`.
- [ ] Clone hot paths: `indexing/mod.rs:960,964` wave retry (index-keyed outcomes), `:1004-1005` commit (move/drain), `markdown.rs:47,71-93` per-window `metadata.clone()` → `Arc<MarkdownEntityMetadata>`.
- [ ] `extraction/code/adapter.rs:76,120,130,161` → `children()` iterator helper over tree-sitter indexed API.
- [ ] Split `index_files` (~237 lines) wave/commit phases; `run_lexical_search` (~193) match-collect/enrich; review `commands/query.rs:120` `too_many_lines` allow.
- [ ] `missing_docs=allow` (~870 items, M8-tracked) docs pass; remove `allow` when CI `RUSTDOCFLAGS="-D missing-docs" cargo doc` clean.
- [ ] Verify: `cargo clippy`, `cargo test`, `cargo doc --no-deps` clean. Commit.

## Explicit non-goals

- No SoA/data-layout rewrite (no profiling evidence; skill forbids it).
- No actor-model rewrite of `Arc<Mutex>` state (sections short, uncontended, never across await).
- No `PhantomData` unit system (newtypes suffice first).
- No `#[non_exhaustive]`/sealed constructors on app-internal types beyond Phase 6 list.

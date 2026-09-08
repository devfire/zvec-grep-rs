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

## MCP (phase H)

- `tools.ts` interactive elicitation for remote-embedding authorization
  (`elicit` + signed `requestState` round trip); the handlers fail closed
  with an `AUTH.REMOTE_EMBEDDING_REQUIRED` error directing the caller to
  `zg auth grant`. Reason: no elicitation channel is wired through rmcp
  here; the codec + in-memory replay guard (`request_state.rs`) are
  complete and tested for the interactive flow to adopt later.
- Managed-rg `extraArgs` forwarded to the real rg binary (invert,
  multiline, engines, threads, encodings, `--threads`, …); rejected with
  `rg command option "--x" is not supported by the MCP tool.` Reason:
  the port searches in-process (`lexical/`) and cannot forward raw
  ripgrep flags; everything mappable onto `LexicalSearchOptions` is
  accepted.
- Per-request `apiKey`/`device`/`endpoint`/`embedding` and index-scoping
  fields (`globs`, `hidden`, `maxDepth`, …) on `zvec_grep_index`;
  rejected when present. Reason: index configuration is daemon-owned;
  accepting-and-ignoring them would silently build the wrong index.
  Search-time index knobs are accepted and refreshes reuse the
  index-time file scope.
- `http-transport.ts` dual modern/legacy servers; one stateful rmcp
  `StreamableHttpService` plus axum pre-checks reproducing the legacy
  observable behavior (unknown session → 404, session-less
  non-initialize POST → 400, session-less GET → 405, cap → 503 with the
  TS `-32000` bodies). Reason: rmcp answers all of these with 401;
  behavior parity lives in the adapter, never in renamed tools.
- `stdio-bridge.ts` daemon-subprocess supervision + elicitation
  forwarding; `run_stdio_server` serves the router in-process.
  Reason: the daemon links the router directly, so there is no child to
  supervise and `shouldStopStdioBridge` has no subject.
- MCP `_meta` trace context (`trace.rs` extracts it); no ambient
  propagation across the async tool handlers. Reason: the context rides
  a thread-local and tokio work-stealing does not preserve it; log
  events from MCP calls carry no trace rather than a wrong one.
- `Range` JSON (`start_line` snake_case), `RgMatch` (`rg_match`) and
  `ContextCoverage` (`RankedSample`) wire shapes; aligned to the TS
  contract (`startLine`, `lexical_match`, `ranked_sample`) with
  `CURRENT_INDEX_VERSION` bumped 1 → 2. Reason: frozen wire formats must
  match TS; v1 indexes are rejected with `VERSION_MISMATCH` directing a
  rebuild instead of misreading persisted ranges.
- `Date.parse` generality for `modifiedAfter`/`modifiedBefore` strings;
  epoch millis, `YYYY-MM-DD` (local midnight), RFC 3339 and
  `YYYY-MM-DD HH:MM:SS` parse, anything else errors with the TS message.
  Reason: chrono cannot cover `Date.parse`'s free-form tail; the
  failure message is identical.
- Fixed while proving H (phase-G latent bug): `JobScheduler::publish`
  used `watch::send`, which since tokio 1.53 drops the value when no
  receiver exists — a job finishing before `wait()` subscribed left the
  slot stale and the waiter hung forever. `publish` uses `send_replace`
  now; `late_waiter_observes_terminal_state` pins it.

## CLI (phase I)

- Hand-rolled `parseArgs` over `process.argv`; clap-derive tree in
  `crates/zg/src/cli.rs`. Reason: idiomatic CLI parsing with `--help`
  and shell completions for free. Frozen surface kept: subcommand names,
  flag spellings, and every `validateCliShape`/`parseCommand` message.
  Generic unknown-flag errors follow clap wording; exit code is 1 for all
  failures (clap's 2 is remapped) and 0 for help/version, matching
  `process.exitCode`.
- CLI plain `Error`s; `CliError` enum with `ZVEC_GREP.ENGINE.CLI.*`
  Rust-addition codes + golden registry. Reason: M1 bans stringly errors;
  user-facing messages stay byte-identical where frozen.
- `@modelcontextprotocol/client` StreamableHTTP + elicitation +
  progress-heartbeat `DaemonClient`; raw JSON-RPC 2.0 over reqwest with
  the same framing the endpoint tests pin. Reason: no rmcp client-HTTP
  feature in the workspace (avoids a second TLS stack); the server fails
  closed with a `zg auth grant` directive instead of eliciting, so there
  is no interactive round trip to forward and `--allow-remote` only mints
  direct-mode one-shot permits. Long calls use a 30-minute bound instead
  of open-ended heartbeats.
- `zvec_grep_search` structured groups over MCP; CLI prints the text
  content our daemon returns, using structured `groupResults` only when a
  server provides them. Reason: the phase-H search tool returns text only;
  the `INCOMPATIBLE_SERVER_SEARCH_MESSAGE` guard stays for genuinely old
  servers.
- Server-mode `zvec_grep_index` with per-request embedding/credentials/
  file-scope knobs; CLI sends only `{root, rebuild, wait, debug}` and
  fails fast when those flags meet server mode. Reason: phase H rejects
  those overrides server-side; forwarding them would only surface the
  rejection later.
- `--line-regexp` forwarded to the rg subprocess; mapped to `^(?:pat)$`
  wrapping. Reason: no subprocess exists; the wrapped regex is what rg
  would match.
- `--max-count` forwarded to the rg subprocess; mapped onto the native
  per-file cap. Reason: same — the engine honors it directly.
- `--case-sensitive` forwarded; accepted as an explicit no-op (the engine
  default). Reason: behavior-preserving without a flag to forward to.
- Exotic short-flag groups (`-in`, …); clap shorts for the common set
  (`-i -w -F -S -x -C -A -B -m -g -t -T -L`) only. Reason: clap cannot
  parse combined short groups with values; the common cases work, the rest
  error instead of misparsing.
- Short output-changing flags (`-c`, `-l`, …); clap generic error instead
  of the tailored managed-rg text (long forms keep the exact TS message).
  Reason: enumerating every short alias as a hidden flag is weight without
  behavior gain; the invocation still fails.
- `query`/`index`/`install` interactive TTY prompts via stdin readline;
  install target prompting replaced by detect-or-require-`--target`.
  Reason: no interactive multi-select UI in scope; `--yes`/`--force`
  cover non-interactive use.
- `AGENTS.md`/`QWEN.md` guidance markdown blocks; not written. Reason:
  plan scopes install to MCP config read-modify-write.
- JSONC target files rewritten via serde (comments lost); refused without
  `--force` when comments are detected. Reason: no JSONC-preserving
  editor dependency; refusing by default protects user files.
- Per-invocation `--device` in direct/`server run` mode; honored via
  `config model set --device` and `$ZVEC_GREP_DEVICE`, not per call.
  Reason: neither `CreateZvecGrepOptions` nor daemon `ServiceConfig`
  carries a device override, and threading one through both crates is
  out of phase scope.
- `zg` keeps its `zg-server` dependency (the plan suggested dropping
  it). Reason: `server run`/`--stdio` serve the router in-process from
  the CLI binary; spawning a separate binary would add a process hop for
  no behavior gain, and direct mode never touches the server stack.
- `--max-filesize` free-form sizes; plain bytes plus `K`/`M`/`G`
  suffixed values. Reason: `parseByteSize` scales without a stated spec
  to mirror; common spellings parse, anything else errors.

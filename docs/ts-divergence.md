# Divergences from the TypeScript implementation

This is the record every `docs/ts-divergence.md` citation in the code
points at. The Rust port mirrors the TypeScript (`zvec-grep`) behaviour,
wire shapes, flag spellings, and user-facing validation messages wherever
they are frozen; the items below are the deliberate differences that
remain. Anything not listed here is intended to match upstream.

## CLI surface (`zg`)

The canonical Rust surface is the clap subcommand tree (`zg query`,
`zg index`, `zg status`, `zg install`, `zg uninstall`, `zg config`,
`zg auth`, `zg server`, `zg help`, `zg version`, `zg completions`), and
it keeps working unchanged. On top of it, `zg` accepts the upstream
search-first flag CLI via a translation layer (`crates/zg/src/cli/translate.rs`)
that rewrites argv **before** clap parses, so both spellings execute the
identical subcommand with identical validation:

| Upstream spelling | Canonical spelling |
| --- | --- |
| `zg <query words…> [search flags…]` | `zg query <query words…> [search flags…]` |
| `zg --index [index args…]` | `zg index [index args…]` |
| `zg --status [status args…]` | `zg status [status args…]` |
| `zg --install [install args…]` | `zg install [install args…]` |
| `zg --config [config args…]` | `zg config [config args…]` |
| `zg --auth [auth args…]` | `zg auth [auth args…]` |
| `zg --server on\|off\|status\|run` | `zg server on\|off\|status\|run` |
| `zg --server --stdio` | `zg server --stdio` |

Remaining CLI divergences and choices:

- **`zg query` vs upstream's search verb.** Upstream treats bare words
  as a search; the word `search` is accepted as a visible alias of
  `query` (`zg search hello` ≡ `zg query hello`), so upstream examples
  and MCP client configs (`zg --server --stdio`) work verbatim.
  `zg serve` stays a hidden removed alias that errors with its
  migration message.
- **Bare `zg` prints the main help and exits 0** (the canonical
  no-subcommand behaviour), rather than erroring as a search with no
  query. A search with no query is only reachable as `zg query` with no
  positional words, which behaves exactly as the canonical subcommand
  always has.
- **Search flags must follow the query words** in the search-first
  form (`zg hello --limit 5`). A leading search flag such as
  `zg --limit 5 hello` is not translated; use the canonical
  `zg query --limit 5 hello`. Leading flags other than the six mapped
  ones (`--help`, `--version`, unknown flags) are left for clap, which
  owns help/version rendering and generic unknown-flag errors; the
  hand-rolled upstream parser's exact help layout and unknown-flag
  texts are not reproduced.
- **Misspelled-command warning (upstream #105).** When the first search
  word is a proper prefix (≥ 3 characters) of a known command name —
  `zg quer …`, `zg stat …` — a warning naming the likely command is
  printed to stderr, and the words still run as a search. This prefix
  rule is the whole heuristic, by choice: edit-distance guesses would
  also nag genuine searches (e.g. `zg stats …` warns about nothing —
  it is not a prefix of `status`), and exact command names never reach
  the search path at all because they dispatch as commands. Spelling
  `zg query …` explicitly suppresses the warning. One-word searches
  are otherwise never blocked or altered.
- **`--flag=value` forms of the six mapped flags** are also expanded
  (`zg --server=run` ≡ `zg server run`); both spellings forward to the
  same subcommand.
- **Cross-flag validation messages** (`validateCliShape` /
  `parseCommand` texts) are byte-identical to upstream; clap's own
  usage-error texts are not.
- **Server-mode `zg index`** sends only `{root, rebuild, wait, debug}`
  to the daemon: per-request credentials and index scoping are daemon
  configuration in this port and fail fast locally instead of tripping
  the server rejection.
- **Output renderers** are idiomatic ports, not byte copies, of the TS
  formatters: agent markdown keeps `<file:range>` headers, scores, and
  previews; the human view keeps labelled fields with optional ANSI
  colour. Plain-text helpers (`oneLine`/truncate equivalents) are pure
  builders with the same shaping intent.
- **`zg install`** writes managed IDE MCP entries and marked blocks
  only; guidance markdown blocks (`AGENTS.md`) that upstream manages
  are out of scope by design.

## Daemon client (`zg` → `zg-server`)

- The CLI speaks raw JSON-RPC 2.0 over reqwest (initialize →
  `notifications/initialized` → `tools/call`, `mcp-session-id` header,
  plain-JSON or SSE bodies) instead of the TypeScript SDK client; the
  TLS stack is the workspace-standard rustls.
- **No elicitation.** Neither the MCP server nor the CLI client wires
  the interactive remote-embedding authorization round trip: without an
  existing grant both fail closed with the `zg auth grant` directive,
  which the CLI surfaces verbatim. The HMAC request-state codec and
  replay guard are implemented and unit-tested, but no caller drives
  the interactive flow yet.

## Engine (`zg-core`)

- **The `ZvecGrepService` facade is synchronous** (`embed` is sync, so
  `zg-core` needs no tokio); async wrapping happens in the daemon via
  `spawn_blocking`. Read sessions are explicit RAII guards; idle-TTL
  eviction belongs to the daemon layer.
- **Standalone storage.** Rust indexes never share collections with the
  TS implementation: file metadata lives in `files.json`, a
  TypeScript-generation `files.zvec` in the same directory is foreign
  and refused loudly (`STORAGE.FOREIGN_TS_INDEX_PRESENT`), never
  migrated or deleted. The on-disk layout version matching the TS
  constant is a shared-origin coincidence, not compatibility.
- **FTS tokenizer is `standard`, not `jieba`.** Since the two builds
  never share collections, the port bakes the `standard` tokenizer into
  `index.zvec` and records it in the embedding fingerprint so a foreign
  collection is refused, not adopted; no jieba dictionary ships.
- **Ranking key order is byte order**, not TS `localeCompare`, for
  non-ASCII paths in the global context ranking tie-break.
- **Context search plans omit `track_entity_id`** per group, matching
  the TS context `searchPlan` call, which also omits it.
- **Model backends' concurrency shapes differ**:
  ONNX leases sessions from a bounded pool behind an
  `Arc<dyn EmbeddingModel>`; llama.cpp gives each loaded model one
  dedicated worker thread holding the model and a single reused
  context, because its inference state may not cross threads.
- **Rust-only error codes.** Where TS throws plain `Error`s, the port
  still assigns typed codes and records them here: the authorization
  target/store codes beside `AUTH.REMOTE_EMBEDDING_REQUIRED`, the
  `QWEN_TEXT_EMBEDDING` fallback prefix for future catalog entries, and
  the daemon codes `ADDRESS_IN_USE`, `UNKNOWN_JOB`, `ALREADY_RUNNING`,
  and `SHUTDOWN_FAILED`.
- **ONNX truncation reporting** is computed honestly: TS
  transformers.js never reports truncation with real tokenizers (its
  `max_length + 1` probe is clamped by `model_max_length`), so the
  vector-parity expectations encode the correct truncated sets rather
  than the golden empty set. The parity gate is cosine ≥ 0.999 (int4
  dequantization kernel noise between ORT Web and ORT native).

## Daemon (`zg-server`)

- **One actor task per root.** Instead of TS's shared mutable maps,
  each root is owned by one actor task holding its `RootRuntime`,
  index coordinator, watch manager, and read-session cache as plain
  `&mut` state (no lock protects them); the runtime manager stores
  only channel handles and join handles, and idle eviction is carried
  by each actor's own deadline rather than a manager task.
- **Cancellation** uses `CancellationToken` at the async edge bridged
  to a sync cancel flag in the index body; dropping a `spawn_blocking`
  handle cannot abort it, so shutdown awaits in-flight work. The TS
  promise-chain generation idiom becomes sequential task execution:
  one task per job, one followup slot per root.
- **Model pool loads run on `spawn_blocking`** with `Notify`-based
  single-flight waiting (waiters share a dehydrated error code) instead
  of shared promises; there is no `dispose()` — eviction drops the last
  `Arc` and destructors run then, with the same residency bound.
- **Process signalling uses `/bin/kill`** (`SIGTERM` then `SIGKILL`)
  instead of `process.kill`, because `libc::kill` is `unsafe` and the
  workspace forbids `unsafe`; escalation is Unix-only and elsewhere a
  refused shutdown surfaces as `SHUTDOWN_FAILED`. The lock machine name
  reads `$HOSTNAME` (falling back to `/proc/sys/kernel/hostname`)
  instead of `os.hostname()`.
- **Watching uses per-directory `notify` watches** like TS on Linux
  (#71): a recursive watch registers ignored trees (`node_modules`,
  `target`) and exhausts inotify quotas, so each indexable directory
  gets a `NonRecursive` watch pruned by the scanner predicate, with
  registration upkeep on the blocking classify thread; the watch
  manager is owned by exactly one root actor.
- **MCP managed-rg surface is reduced**: the real-rg `extraArgs`
  (invert, multiline, engines, threads, encodings) are rejected because
  search runs in-process and cannot forward raw ripgrep flags.
  Index-scoped knobs accepted at the MCP boundary (`hidden`,
  `no_ignore`, `ignore_files`, `max_depth`, `max_file_size_bytes`,
  `follow`, `embedding_concurrency`) apply at index time; refreshes
  reuse the index-time file scope.
- **The stdio MCP bridge serves the tool router in-process** — there is
  no daemon subprocess to supervise and no elicitation to forward, so
  TS's `shouldStopStdioBridge` has no subject here.

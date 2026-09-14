# Rust Hardening Action Plan — Round 2

Source: post-`2176937` audit against `rust-defensive-programming` (11 pts) + `advanced-rust-patterns` (10 pts) + correctness/elegance review, 2026-09-14.
Follows `docs/rust-hardening-plan.md` (round 1, phases 0–8). Round 1 landed the lint firewall and the slicing/wildcard/`must_use` sweeps; this plan covers what round 1 missed, what it left unfinished, and what the module split in `e18443b` introduced.

Rule: no behavior change except where noted (R1 portability fix, R2 TTL fix, R6 error-type refactor). Each phase commits separately.

## Baseline (verified, not assumed)

| Gate | Status |
| --- | --- |
| `cargo clippy --workspace --all-targets` (cold target dir) | clean |
| `cargo test --workspace` | 283 tests, 0 failed |
| `cargo fmt --all --check` | **fails — 15 diffs across 8 files** |
| `cargo check` on any non-Linux Unix | **fails — E0453, see R1** |
| MSRV 1.88 as declared | **unverified — CI only builds `stable`** |

What round 1 already got right and this plan does not touch: zero `anyhow` in library code, zero `Box<dyn Error>`, zero `for i in 0..len`, zero `..Default::default()` outside tests, `unsafe_code = "forbid"`, `deny` on `indexing_slicing` / `wildcard_enum_match_arm` / `unwrap_used` / `expect_used` / `panic` / `must_use_candidate` / `missing_errors_doc` / `missing_panics_doc`, a real actor protocol in `zg-server/src/backend/actor.rs` (`RootCommand` + `oneshot` replies, sender `pub(crate)`), 19 domain newtypes, and 64 `_ =>` arms that are all guard-pattern matches where exhaustiveness is genuinely impossible.

---

## Phase R1 — `zg-core` does not compile on macOS or BSD (P0, blocking)

`crates/zg-core/src/utils/lock.rs:365-390`:

```rust
#[cfg(all(unix, not(target_os = "linux")))]
{
    // SAFETY: `kill` with signal 0 performs no action ...
    #[allow(unsafe_code)]                      // ← E0453
    let result = unsafe { libc::kill(pid as i32, 0) };
```

`unsafe_code = "forbid"` in `[workspace.lints.rust]` reaches rustc as `-F unsafe_code`. **`allow` cannot override `forbid`.** Reproduced standalone and in-tree by flipping the `cfg`:

```
error[E0453]: allow(unsafe_code) incompatible with previous forbid
  = note: `forbid` lint level was set on command line (`-F unsafe_code`)
error: usage of an `unsafe` block
```

Invisible today only because the Linux arm `cfg`s the block out and CI is `ubuntu-latest`-only.

- [ ] Replace the `libc::kill(pid, 0)` arm with a safe wrapper so `forbid` survives intact: add `rustix` (feature `process`) and call `rustix::process::test_kill_process(Pid::from_raw(pid)?)`, mapping `Errno::PERM` to "alive" exactly as today. Do **not** downgrade the workspace lint to `deny` — that weakens a real guarantee to paper over a portability bug.
- [ ] Delete the now-stale doc sentence "that call is the sole `unsafe` in the crate" (`lock.rs:365`); after this phase the crate has none.
- [ ] `.github/workflows/ci.yml`: add `macos-latest` to the `test` and `clippy` jobs (matrix over `runs-on`). This is the gate that would have caught it.
- [ ] Verify: `cargo clippy --workspace --all-targets` clean on Linux **and** macOS; `cargo test --workspace` green on both. Commit.

## Phase R2 — `now_ms` ×8, and the auth TTL that fails open (P0, behavior change)

`crates/zg-server/src/mcp/request_state.rs:314`:

```rust
fn now_ms() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64).unwrap_or(0)
}
```

consumed at `request_state.rs:257`:

```rust
if now_ms().saturating_sub(envelope.issued_at_ms) > self.ttl.as_millis() as u64 {
```

Failure scenario: wall clock set before the epoch → `duration_since` errors → `now_ms() == 0` → `0.saturating_sub(issued_at) == 0` → never exceeds the TTL → **request-state tokens never expire.** Low likelihood, wrong direction: a fail-open in a signature/TTL path.

The same function is copy-pasted byte-identically eight times, while `zg-core` already ships `pub struct UnixMillis(i64)` (`crates/zg-core/src/types/mod.rs:45`) that none of them use.

- [ ] Add one authority in `zg-core` next to `UnixMillis`: `pub fn now() -> Option<UnixMillis>` (or `EngineResult<UnixMillis>`), never a `0` sentinel.
- [ ] Delete all eight copies and route through it: `zg-server/src/mcp/request_state.rs:314`, `mcp/tools.rs:761`, `read_session_cache.rs:199`, `mcp/progress_heartbeat.rs:82`, `server_controller.rs:533`, `backend/util.rs:42`, `model_pool/state.rs:124`, `job_scheduler/state.rs:95`.
- [ ] `request_state.rs:257`: treat an unavailable clock as **expired**, not fresh. Preferred: capture a monotonic `Instant` at issue time and compare on that, leaving the wall-clock `issued_at_ms` as metadata only.
- [ ] Audit the other seven call sites for the same `0`-sentinel direction bug (job dedupe windows, lease TTLs, heartbeat intervals) and state each one's chosen failure direction in a comment.
- [ ] Verify: new test injecting a pre-epoch clock (or an injected `now` fn) asserts `verify()` rejects; `cargo test -p zg-server` green. Commit.

## Phase R3 — Module-split debris from `e18443b` (P1, pure deletion)

### Duplicated helpers

- [ ] `fn lock<T>(&Mutex<T>) -> MutexGuard<'_, T>` — **five verbatim copies**: `zg-server/src/http_server.rs:371`, `index_coordinator.rs:153`, `mcp/request_state.rs:321`, `runtime_manager.rs:225`, `job_scheduler/state.rs:89`. Collapse to one `trait MutexExt { fn lock_ignore_poison(&self) -> MutexGuard<'_, T>; }` in a `zg-server/src/sync.rs` (or `zg-core::utils`).
- [ ] **Sixth copy that diverges semantically**: `zg-core/src/pipeline/indexing/progress.rs:95` — `lock_stats` returns `IndexStats::default()` on poison while `lock_stats_mut` (`:99`) returns `into_inner()`. Same intent, two behaviors; the `default()` one silently zeroes progress instead of reading through. Round 1 Phase 1 listed this as "poisoned-mutex → typed error, not zero stats" and it did not land. Pick one behavior (prefer `into_inner()`) or propagate; do not keep both.
- [ ] `fn to_hex` — **four copies, each with a `format!` allocation per byte**: `zg-core/src/authorization/store.rs:47`, `authorization/target.rs:47`, `utils/hash.rs:17`, `zg-server/src/logger.rs:205`. One helper, `use std::fmt::Write; write!(hex, "{byte:02x}")?` or a nibble lookup table. (Round 1 Phase 8 listed two of the four.)
- [ ] `fn constant_time_eq` — **two divergent copies** (`zg-core/src/authorization/store.rs:68` explicit loop, `zg-server/src/http_server.rs:361` `fold`). See R4.
- [ ] `zg-core/src/pipeline/indexing/scanner/walk.rs:74` and `:196` — the same six-line `let followed = match (&target, root.follow.unwrap_or(false))` symlink-follow block twice. Extract.

### Dead stubs that exist only to keep an import "used"

- [ ] Delete `fn unused_time_guard(_: SystemTime) {}` — `zg-core/src/utils/lock.rs:399-400`, `#[allow(unused)]`.
- [ ] Delete `fn zvec_base_delay_ms()` — `zg-core/src/storage/zvec/mod.rs:659-662`, `#[allow(dead_code)]`.
- [ ] Delete `fn cancel_flag_for()` — `zg-server/src/backend/util.rs:53-56`, `#[allow(dead_code)]`.
- [ ] Drop the now-unused imports each was shielding, rather than re-adding a stub.
- [ ] `zg-server/src/job_scheduler/dedupe.rs:110`: `pub(crate) fn sort_queue(state: &mut Inner, #[allow(unused_variables)] shared: &Shared)` — the parameter is unused. Remove it from the signature and its call sites (an `#[allow]` on a parameter is not a fix).
- [ ] Verify: `cargo clippy --workspace --all-targets` clean with **zero** `dead_code` / `unused` / `unused_variables` allows remaining in `src/`; `cargo test --workspace` green. Commit.

## Phase R4 — Crypto primitives: stop hand-rolling (P1)

- [ ] Replace both `constant_time_eq` copies with `subtle::ConstantTimeEq` (`subtle` is already in the tree transitively via `hmac`; promote it to an explicit `[workspace.dependencies]` entry). Neither hand-rolled version uses `black_box` or volatile reads, so nothing stops LLVM from short-circuiting the difference accumulator — the property the function's name asserts is not actually guaranteed.
- [ ] Keep the existing length-inequality early return (length is not secret) but state that explicitly in the doc comment.
- [ ] `zg-server/src/mcp/request_state.rs:283` already does this correctly via `mac.verify_slice(tag)` — use it as the reference and cross-link from the new helper.
- [ ] `zg-server/src/mcp/request_state.rs:254`: `envelope.method != "tools/call"` is a stringly-typed binding check inside a security path. Make it a `#[non_exhaustive] enum BoundMethod` (or at minimum a `const`), so a typo cannot silently widen the binding.
- [ ] Verify: existing `request_state` tamper tests green (`token_is_rejected_when_tampered`, `binding_rejects_other_principal`); add a length-mismatch case. Commit.

## Phase R5 — CI gates that should already exist (P1, no code change)

- [ ] `cargo fmt --all --check` job. Currently **15 diffs across 8 files**: `zg-core/src/pipeline/indexing/{context,progress,retry}.rs`, `zg-core/src/pipeline/search/recall.rs`, `zg-server/src/mcp/input_normalization/{paths,rg_args,rg_scan,search}.rs`. Commit the fmt sweep separately from the gate so the diff stays reviewable. (Git log shows fmt being applied per-file by hand — `3f7cdde`; that's what the missing gate buys you.)
- [ ] MSRV job: `dtolnay/rust-toolchain@1.88` + `cargo check --workspace`. `rust-version = "1.88"` carries a six-line justification in `Cargo.toml` and is never actually tested — CI only builds `stable`.
- [ ] Add `--locked` to every `cargo test` / `cargo clippy` invocation so `Cargo.lock` drift fails CI instead of silently resolving.
- [ ] **Delete the vacuous error-code guard.** `.github/workflows/ci.yml`, `error-codes` job:
  ```yaml
  test -z "$(grep -rn 'EngineErrorCode::new(&format!' crates/ || true)"
  ```
  `EngineErrorCode::new` does not exist — the constructor is `from_static`. This guard has been passing vacuously since the rename. Either fix the pattern to `from_static(&` / `from_static(format!` or, better, delete it once R6 makes the grep structurally unnecessary.
- [ ] Verify: all four jobs green on Linux + macOS. Commit.

## Phase R6 — The error taxonomy (P1, the architectural one)

This is the largest item and the one that decides whether the codebase reads as *disciplined* or as *architecturally serious*. Right now the lint config is doing work the type system should be doing.

**Current state.** `EngineError` is `{ code: EngineErrorCode, message: String, context: Option<String> }` where `EngineErrorCode` wraps `&'static str`. Every one of ~250 `EngineResult<T>` signatures in the public `zg-core` API returns it. There are 127 direct `EngineError::new(...)` sites and **174 distinct code literals**. `ModelError` and `AuthError` are proper `thiserror` enums, but they are islands: only two `From` impls lift into `EngineError`.

Three concrete defects:

1. **The doc comment is false.** `zg-core/src/error.rs:15` claims codes are "exhaustiveness-checkable at the call site." `EngineErrorCode` is a struct wrapping `&'static str` — a `match` on it can never be exhaustive — and `from_static` is `pub`, so the set is not even closed to downstream crates. A caller who wants to retry on lock contention but not on a failed model download must string-compare suffixes.
2. **Zero `source()` impls in the entire workspace**, and only three `#[from]` (`zg/src/error.rs:83,86`). Every `io::Error` / `serde_json::Error` / HTTP failure is flattened into a `String` at the conversion site. Round 1 Phase 3 listed "implement `source()` forwarding on all six" — none landed.
3. **The golden registry covers 86 of 174 codes**, and the CI grep meant to cover the gap is the dead one from R5. The golden file's own scope note admits the storage/indexing/search/scanner literals are unpinned.

Steps, smallest-blast-radius first:

- [ ] Fix the false claim in `zg-core/src/error.rs:15` immediately, as a one-line docs commit, independent of the refactor.
- [ ] Add `source: Option<Box<dyn std::error::Error + Send + Sync>>` to `EngineError` plus a `with_source` builder, and implement `fn source(&self)`. Do the same for `BackendError`, `SessionError`, `AcquireError`, `McpError`, `DaemonError`.
- [ ] Thread real sources at the highest-traffic conversion sites first (`utils/json_io`, `storage/zvec`, `models/download`) rather than all 127 at once.
- [ ] **Un-dead the `--debug` cause printer.** `zg/src/format/error.rs:19-25` walks `source()` and its module doc promises "the full cause chain". With zero `source()` impls the loop is dead, and for the two `#[error(transparent)]` variants (`CliError::Engine`, `CliError::Daemon`) it is worse than dead — `Display` and `source().unwrap().to_string()` are the same string, so `--debug` prints:
  ```
  error: ZVEC_GREP.ENGINE.X: msg
  code: ZVEC_GREP.ENGINE.X
  caused by: ZVEC_GREP.ENGINE.X: msg     ← verbatim duplicate
  ```
  Skip the first `source()` hop for transparent variants, and add a test asserting the chain is >1 level deep and non-duplicating.
- [ ] Convert `EngineErrorCode` from `struct(&'static str)` to `#[non_exhaustive] enum` with `const fn suffix(self) -> &'static str`. Same wire strings, one literal per variant, codes become genuinely matchable, `from_static` stops being a public escape hatch, and the golden test becomes a compiler-checked `match` over variants instead of a hand-maintained list plus a grep. ~174 construction sites; land it per-module behind a temporary `from_static` shim, then delete the shim.
- [ ] Once the enum lands: regenerate `tests/golden/error-codes.txt` from an exhaustive variant walk, delete the scope note, and delete the `error-codes` grep job.
- [ ] Verify: `cargo test --workspace` incl. both golden registries; `zg --debug` prints a multi-level, non-duplicated chain; `cargo doc --workspace --no-deps` clean with `RUSTDOCFLAGS=-D warnings`. Commit per module.

## Phase R7 — Skill-checklist gaps round 1 skipped (P2)

- [ ] **Sealed traits (advanced #5).** Five `pub trait`s, none sealed or `#[non_exhaustive]`: `EmbeddingModel` (`zg-core/src/models/mod.rs:95`), `RankingModel` (`models/embeddings.rs:305`), `LanguageAdapter` (`extraction/code/adapter.rs:207`), `WorkspaceIndexStorage` (`storage/mod.rs:91`), `ClosableHandle` (`zg-server/src/read_session_cache.rs:23`). Adding a method to any of them is currently a breaking change on a published crate. Seal `WorkspaceIndexStorage`, `LanguageAdapter`, and `ClosableHandle` via `mod private { pub trait Sealed {} }`. For `EmbeddingModel` / `RankingModel`, decide whether third-party backends are a supported extension point and **document the answer either way** — an unsealed trait with a stated stability promise is fine; an unsealed trait by accident is not.
- [ ] **`Cow` (advanced #9) — zero occurrences workspace-wide.** The poster child is `zg-core/src/error.rs:274 redact_error_text`, which runs on every error and allocates **four times on the common no-secret path**: `redact_url_userinfo` builds a `String::with_capacity` unconditionally, then three `replace_all(..).into_owned()` calls each allocate even when the result is `Cow::Borrowed`. Change to `fn redact_error_text(&str, usize) -> Cow<'_, str>` and have `redact_url_userinfo` return `Cow` too; chain the three regex passes without forcing ownership.
- [ ] **`bool` pairs (defensive #10).** Three CLI printers sit just under the `fn_params_excessive_bools` threshold of 3, so the lint stays green while the hazard remains: `zg/src/format/control.rs:4 print_control_status(running: bool, ready: bool, ..)`, `format/error.rs:12 print_error(.., color: bool, debug: bool)`, `format/context.rs:14 print_context_result(.., human: bool, color: bool)`. `(human, color)` is exactly the pair a refactor silently transposes. Introduce `enum Color { Always, Never }` and `enum Rendering { Human, Agent }`; consider lowering `fn_params_excessive_bools` to `2` afterwards so this cannot recur.
- [ ] **Total order on floats.** `zg-core/src/pipeline/search/fusion.rs:124` uses `partial_cmp(..).unwrap_or(Ordering::Equal)`. The scores are finite (`1.0/(RRF_K + rank)` sums) so it is correct today, but `f64::total_cmp` gives a real total order with no papered-over arm — and an inconsistent comparator can make `sort_by` panic outright on current std if the NaN-free invariant ever breaks.
- [ ] Verify: `cargo clippy`, `cargo test --workspace`, `cargo doc --no-deps` clean. Commit.

## Phase R8 — Documented invariants on byte-offset arithmetic (P2, docs only)

Several extractors mix byte-offset scanning with `&str` slicing. `clippy::indexing_slicing` does **not** cover `str` range indexing (that is `clippy::string_slice`, a restriction lint not in `all`), so these sites pass the firewall while carrying an un-stated UTF-8 boundary invariant.

- [ ] `zg-core/src/extraction/code/extractor/script_blocks.rs:159` `&text[after..tag_end]` and `:174` `text[start_offset..close]`. These are in fact safe — every offset derives from a match on an ASCII byte (`<`, `>`, `s`), and no ASCII byte can appear as a UTF-8 continuation byte — but nothing in the code says so. Add the one-sentence invariant comment, or route through `text.get(a..b)` with a `let-else`.
- [ ] `zg-core/src/utils/glob.rs:155-163` `pattern[start_index + 1..end_index]` and `&content[1..]`. Same reasoning (`[`, `]`, `!`, `^` are ASCII); same missing comment.
- [ ] Consider enabling `clippy::string_slice = "warn"` so new sites of this shape surface for review rather than relying on the reviewer noticing the `indexing_slicing` blind spot.
- [ ] Verify: `cargo clippy --workspace --all-targets`; extractor tests green. Commit.

---

## Explicit non-goals (unchanged from round 1 unless noted)

- No SoA / data-oriented rewrite — still no profiling evidence; the skill forbids applying it on vibes.
- No further actor conversion of the remaining `Arc<Mutex<_>>` state. The sections are short, uncontended, and never held across `.await`; `read_session_cache.rs` already uses `tokio::sync::Mutex` where a lock does span an await. Reviewed and deliberately kept.
- No typestate (advanced #3). `ReadSession` and the `ServerController` start/ready/stop lifecycle are the only candidates, and forcing a generic state parameter onto them would be over-engineering — the skill says so itself. Do not add it to score points.
- No `PhantomData` unit system — the 19 existing newtypes carry the weight; R2 extends `UnixMillis` rather than inventing a dimension layer.
- No `#[non_exhaustive]` on application-internal types beyond the public error enums in R6 and the traits in R7.

## Suggested ordering

R1 → R5 → R2 → R3 → R4 → R7 → R8 → R6.

R1 and R5 first: R1 is the only item that makes the repo *broken* rather than *imperfect*, and R5's fmt/MSRV/macOS gates keep every later phase honest. R6 is last because it is the largest diff and everything above it is independent of the outcome.

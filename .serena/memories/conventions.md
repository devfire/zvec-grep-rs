# Conventions

## Mandatory Skills
- MUST read `skill://rust-defensive-programming` + `skill://advanced-rust-patterns` before any code change (write/review/refactor); both together = production-grade bar.
- `rust-defensive-programming`: no index without bounds proof, no `..Default::default()` with new fields, no wildcard `_` hiding new variants, no boolean params, no fallible `From` conversions, no `unwrap`/`expect` in engine/server paths.
- `advanced-rust-patterns`: no `Arc<Mutex>` without concurrency-architecture justification, no manual index loops, no `unsafe` without `// Safety:` comment, no `anyhow` in library code, no public trait without sealing consideration, no premature SoA/micro-opt without profiling evidence.

## Rust Idioms & Type Safety
- Newtype pattern: Use domain-specific newtypes (e.g., `FileId`, `EntityId`, `WorkspaceId`) instead of raw strings or numeric IDs.
- Error handling: Never call `unwrap()` or `expect()` in production engine or server paths. Bubble errors via `thiserror` (`EngineError`, `EngineResult`) or domain error enums.
- Zero-cost abstractions & performance: Avoid needless allocations or string clones in hot loops (lexical grep, tree-sitter AST traversals, vector codec).
- Defensive programming: Explicit match exhaustiveness, validate file bounds, clamp timeouts/budgets, use atomic operations for file/lease locking.
- Memory safety: Avoid `unsafe` code unless strictly wrapped and audited at FFI boundaries (e.g. `zvec-rust`, `libc`).

## Code Organization
- Match the original TypeScript architecture domain boundaries:
  - `types/`: Pure domain records, identifiers, search/index contracts
  - `utils/`: Reusable filesystem locking, leases, timing, and JSON IO
  - `storage/`: Zvec schema, codec, vector filtering, and index layouts
  - `extraction/`: Language AST extractors and file chunking
  - `models/`: Model catalog, downloaders, embedding backends
  - `lexical/`: Grep-based search pipelines
  - `pipeline/`: Scanner, index scheduler, and search fusion
  - `service/`: High-level workspace index coordinator
- Facade splits: oversized modules become `name/` dir + slim re-export facade (`cli.rs` → 93 LOC + 12 modules); moves are pure, zero behavior change
- Feature-gated backends: `#[cfg(feature)]` backend modules + factory `BackendUnavailable` fallback; default build stays hermetic (no C++ toolchain)
- Rustdoc: never link private items from public docs (deny warnings); use literal code spans

## Root-Scope Policy Flags
- Shape: `Option<bool>` beside `follow` in `RootPath` + `ZvecGrepIndexOptions`, serde `default` + `skip_serializing_if = "Option::is_none"`; `None`/`Some(false)` opt out, `Some(true)` opts in.
- Manifest: raw-JSON `is_optional_boolean` validation precedes serde (missing/null/bool ok; string/number/object → `MANIFEST.INVALID`); manifest key camelCase, status key snake_case.
- Scanner: early-return on opt-in before allocating/probing; never bypass other filters (ignore, hidden, depth, size, symlink, `.git`/`.zvec-grep` hard-skips).
- CLI: `bool_flag` wiring for both root/options literals; reject with `--drop` and in server/auto-server dispatch (single rejection site).
- MCP: `IndexInput` per-request overrides rejected via `reject_index_overrides`; `RootPathOutput` copies field with exhaustive destructure (no `..`) so omitted fields fail compilation.
- No new policy enums, scanner params, traits, or locks; assign new field explicitly in every touched literal, never hide with `..Default::default()`.

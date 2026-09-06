# Conventions

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

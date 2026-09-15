# .NET Structured Support (C# + VB) — v4 Plan (incorporates destructor + step-2 feedback)

## Context

v3 plan shape (deps → shared family fixes → file_type → formats → two java.rs-clone adapters → wiring → name-asserting tests) is sound and its anchors verified exact this session. Review feedback blocks v3 as written: `destructor_declaration` misclassifies as `Class` through the shared `classify_code_node` substring heuristics (`walk.rs:153,169-170`), `push_unique` is private (`metadata.rs:206`), the VB haystack rebuild is unstated, C#/VB events are asymmetrically logged, `delegate_declaration → Value` is unlogged, and the step-5 scope comment misstates which string is the scope. End state after this plan: `.cs` structured via a C# adapter with an 11-kind name-safe table (destructor dropped); `.vb` structured via a VB adapter with a name-safe table plus `New` and VB-modifier overrides; F# stays text fallback; every listed kind is name-and-type asserted so silent-wrongness cannot ship green.

## Approach

### 0. Add grammar deps

- In `Cargo.toml` `[workspace.dependencies]`, add `tree-sitter-c-sharp = "0.23"`, `tree-sitter-vb-dotnet = "0.1"` (reconfirm exact patch with `cargo add --dry-run` at implementation time, tree-sitter `0.25.10`).
- In `crates/zg-core/Cargo.toml` add both as `.workspace = true`, mirroring the existing `tree-sitter-*` lines.
- Before writing any adapter, read each crate's `NODE_TYPES` from its source and derive every kind table below with keep-only-if-present: a kind stays only if present in `NODE_TYPES`. No `tree-sitter-fsharp`; `.fs/.fsi/.fsx` keep today's unknown-extension text fallback. No new features.

### 1. Fix the shared family hooks (gates VB, low-risk to others)

All edits in `crates/zg-core/src/extraction/code/families/metadata.rs` (`extract_preceding_doc` ~line 70, `extract_common_modifiers` ~line 107):

- `extract_preceding_doc`: when walking `prev_named_sibling`, skip (do not break on) nodes of kind `"blank_line"` only; all other non-comment kinds still break. `blank_line` appears in no other bundled grammar's `node-types.json`, so other languages are unaffected.
- `extract_common_modifiers`: lowercase each `word` before matching (`word.to_ascii_lowercase()`); add NO new keyword arms here. In particular do NOT add `"shared"`/`"friend"` globally — `friend class Foo;` is genuine C++ and would falsely report `Internal`.
- Log both changes in `docs/ts-divergence.md` (one line each: TS symbol, Rust shape, reason): `extractPrecedingDoc` blank_line skip (vb-dotnet emits named `blank_line` after every comment) and `extractCommonModifiers` case-insensitive match (VB `Public`/`Shared` capitalisation).

### 2. VB-only modifier synonyms (new `vb.rs` hook, not shared)

- In the new `crates/zg-core/src/extraction/code/languages/vb.rs`, override `extract_modifiers` (only this hook diverges from the java.rs mirror): rebuild the haystack exactly as `extract_common_modifiers` does — `extract_generic_signature(node).or_else(|| node.text().map(first_non_empty_line))`, all three helpers already `pub` — then call `extract_common_modifiers(node)` (equivalently `name_field_extract_modifiers(node)`), then scan the rebuilt haystack case-insensitively for VB-only spellings and push `Static` for `shared`, `Internal` for `friend`.
- Dedup locally with `if !mods.contains(&m) { mods.push(m) }`. Do NOT call `push_unique` — it is private (`fn push_unique`, `metadata.rs:206`) and `vb.rs` cannot reach it. No other adapter changes; `c_lang.rs`, `cpp.rs`, `rust.rs`, `python.rs`, `js_ts.rs`, `name_field.rs` call sites are untouched and `friend` never reports `Internal` outside VB.

### 3. Recognize VB extensions

- In `crates/zg-core/src/file_type.rs` `CODE_EXTENSIONS`, add `("vb", "vb")`. `.cs → csharp` already exists.
- `--type` resolution needs no change (`alias_for` → `ignore` builtin table; both work today). No change to `RIPGREP_FILE_TYPE_ALIASES`.
- Extend `file_type.rs` tests: `detect("a.vb")` → `(Code, "vb")` plus case-insensitivity. `.csx/.cshtml/.razor/.xaml/.csproj/.sln` and all `.fs*` stay text-fallback by design.

### 4. Declare formats structured (truthfulness, not a behavior gate)

- In `crates/zg-core/src/code_formats.rs` `STRUCTURED_CODE_FORMATS`, add `"csharp"`, `"vb"` alphabetically; extend the `membership` test.
- Known non-gate (state in a code comment, do not claim otherwise): `is_structured_code_format` has no callers outside its test; the real gate is `resolve_adapter` + `has_grammar` + `language_for_format` (entry.rs falls back to text when any misses).

### 5. C# adapter — restricted name-safe table, destructor dropped

- Create `crates/zg-core/src/extraction/code/languages/csharp.rs`; register `pub mod csharp;` in `languages/mod.rs`. Mirror `languages/java.rs` exactly (`ENTITY_TYPES`/`SCOPE_TYPES` consts, `static CSHARP_ADAPTER`, `struct CSharpLanguage`, `impl Sealed`, `impl LanguageAdapter` with `format() -> "csharp"`, all four hooks delegating to `name_field_extract_*`, `classify_node` returns `None` — the shared `classify_code_node` substring heuristics already land for every kept kind).
- Decision (adopts feedback option 1, drop — no `classify_node` override): `destructor_declaration` is EXCLUDED. Verified this session against `extractor/walk.rs:153,169-170`: `"destructor_declaration".contains("constructor")` is false so the `Function` arm never fires, execution falls through to `contains("struct")` (de·struct·or) which returns `Class`. The destructor's `name` field is the type name, so `~Greeter()` would emit `(Greeter, Class)`, byte-identical to the class entity. Finalizers are rare and discouraged in modern C#; dropping keeps `classify_node → None` honest and matches the plan's when-in-doubt-exclude posture. Re-adding requires a `classify_node` override mapping it to `Function` plus fixture rows — a new plan, not a drive-by.
- `ENTITY_TYPES` = keep-only-if-present subset of (no additions without a `NODE_TYPES` hit plus a name-and-type-asserted fixture row in step 8): `class_declaration, struct_declaration, interface_declaration, enum_declaration, record_declaration, method_declaration, constructor_declaration, property_declaration, delegate_declaration, namespace_declaration, file_scoped_namespace_declaration`. `record_struct_declaration` must NOT appear (absent from `NODE_TYPES`).
- Explicitly EXCLUDED (decision, not oversight): `destructor_declaration` (per above), `indexer_declaration` (no `name` field → `None`), `operator_declaration` (no `name` field; identifier-fallback returns the return type, e.g. `K` for `public static K operator +(K a, K b)` — silently wrong, no fallback override without a dedicated operator-name mapping, out of scope), `conversion_operator_declaration` (same hole), `local_function_statement` (unreachable: `walk_code_node` recurses only when `!is_entity` or via the scope branch, and `method_declaration` is an entity but not a scope, so the walker never enters a method body — listing it is dead weight; getting it would require making methods scopes and would change breadcrumbs for every language, out of scope), `event_declaration` + `event_field_declaration` (rare `event X { add{} remove{} }` form listed in v2 but the common `public event EventHandler E;` form is `event_field_declaration` with no fields and the name nested in a `variable_declarator`; dropped for symmetry with `field_declaration`, which `java.rs` also skips).
- Accepted asymmetry (state in a code comment on `ENTITY_TYPES`): VB keeps `event_declaration` because vb-dotnet's node bears a `name` field, while C# drops both event forms per above. Without the comment a future reader will "fix" the asymmetry back into the C# name hole.
- Accepted mapping (state in a code comment on `ENTITY_TYPES`): `delegate_declaration → Value` in both languages (verified against the heuristic fallthrough; `CodeSymbolType` has no closer fit — `Alias`/`Interface` would overclaim — and `Value` is harmless).
- `SCOPE_TYPES`: `namespace_declaration, class_declaration, struct_declaration, interface_declaration, enum_declaration, record_declaration`. Exclude `file_scoped_namespace_declaration` from scopes (verified sibling, not parent — scoping on it empties breadcrumbs; still indexed as an entity). Accepted inconsistency (state in a code comment on `SCOPE_TYPES` with this exact wording): block-namespace files yield members scoped as `A.B::Class` (the class's own scope is `A.B`); file-scoped-namespace files (the .NET 6+ default template) yield members scoped as bare `Class`.
- No per-adapter `extract_name` override in `csharp.rs`.

### 6. VB adapter — name-safe table plus `New` override, `namespace_block` scoped

- Create `crates/zg-core/src/extraction/code/languages/vb.rs`; register `pub mod vb;`. Same java.rs mirror with `format() -> "vb"`, `classify_node` → `None` (`class_block`→Class, `module_block`→Module via existing substring heuristics), docs/signature reuse the name-field family.
- `ENTITY_TYPES` = keep-only-if-present subset of: `class_block, module_block, interface_block, structure_block, enum_block, namespace_block, method_declaration, constructor_declaration, property_declaration, delegate_declaration, event_declaration`. Derive from vb-dotnet `NODE_TYPES` at implementation time; each must be in the grammar's name-bearing set. Explicitly excluded: `variable_declarator`, `dim_statement` (field-like locals; symmetry with java.rs skipping fields), `const_declaration`, `enum_member` (named but value-like; out of scope for v1 — do not add without a fixture row). Do NOT list the `type_declaration` wrapper as an entity (it is the parent of the `*_block` kinds, not an entity itself).
- `constructor_declaration` override (the only `extract_name` divergence in either adapter): vb-dotnet `constructor_declaration` has no `name` field (children are only modifiers/parameter_list/statement), so override `extract_name` to return `Some("New")` when `node.kind() == "constructor_declaration"` (VB constructors are always `Sub New`), else `name_field_extract_name(node)`. Unit-test the override directly.
- `SCOPE_TYPES`: `namespace_block` (required — without it VB breadcrumbs lose the namespace and the namespace isn't indexed) plus the type-level blocks confirmed in `NODE_TYPES`: `class_block, module_block, interface_block, structure_block, enum_block` (keep-only-if-present each).
- `extract_modifiers` override per step 2; `extract_doc`/`extract_signature` stay `name_field_*` (docs fixed globally by the step-1 `blank_line` skip, no per-adapter doc workaround).

### 7. Wire adapters and grammars

- In `extraction/code/adapter.rs` `resolve_adapter`, add `"csharp" => Some(&...::csharp::CSHARP_ADAPTER)`, `"vb" => Some(&...::vb::VB_ADAPTER)`. Extend the adapter test near the `resolve_adapter("ruby").is_none()` assertion.
- In `extraction/code/extractor/entry.rs`, extend `has_grammar` with `"csharp" | "vb"`, and `language_for_format` with `"csharp" => tree_sitter_c_sharp::LANGUAGE.into()`, `"vb" => tree_sitter_vb_dotnet::LANGUAGE.into()` (same `.into()` pattern as existing arms).
- No other format-gated sites change (`enrichment.rs` is `FileKind`-based; `prepare.rs` images only; `script_blocks.rs` vue/svelte only; `vector_content.rs` metadata-generic).

### 8. Prove extraction with table-driven name-and-type-asserting tests (the guard, correctly scoped)

- Rule: for EVERY kind in each adapter's `ENTITY_TYPES`, the fixture for that language must contain an instance of that kind AND the test must assert both its `symbol_name` (`Some`, exact expected string) and its `symbol_type` (exact expected `CodeSymbolType`). Name-only assertion is rejected — it misses the destructor class of bug (right name, wrong type). A fragments-nonempty assertion alone is rejected. Adding a kind without a fixture row is rejected in review. Entities with `name: None` still emit fragments, so this is the only guard against nameless-structured-chunks (worse than text fallback) shipping green.
- C# test (in `entry.rs` `#[cfg(test)]`, following the `rust_file("f")` helper pattern): single fixture exercising all 11 kept entity kinds (block namespace + file-scoped-namespace sample, class, struct, interface, enum, record with property, method, constructor, delegate, plus namespace) — or two fixtures if one file cannot hold both namespace idioms — parsing with zero error nodes; assert each expected `(symbol_name, symbol_type)` pair exact, including `delegate_declaration → Value`. Negative assertions: `ENTITY_TYPES` contains none of `destructor_declaration`, `indexer_declaration`, `operator_declaration`, `conversion_operator_declaration`, `local_function_statement`, `event_declaration`, `event_field_declaration` (locks the step-5 drops; re-adding any requires the name/type override plus fixture rows).
- VB test: fixture with `Namespace`, `Class Greeter` + `Sub SayHello` (preceding comment + `Public` modifier), plus one instance of each other listed kind (`Module`, `Interface`, `Structure`, `Enum`, `Property`, `Delegate`, `Event`, `Sub New` constructor); assert every expected name including constructor `Some("New")` plus its `symbol_type`, plus doc extraction across an intervening `blank_line` and `Public`/`Shared` modifier assertions.
- Step-1/2 unit coverage (in `families/metadata.rs` tests or entry tests): VB `Public`/`Shared` case-insensitive modifier extraction through the VB hook only (plus a C++ `friend class Foo;` negative: no `Internal` via the shared helper); doc extraction across an intervening `blank_line`.
- One negative test per format: empty/whitespace file falls back without error. Deterministic, no on-disk fixtures.
- Per-language tie-down: step 5/6 zero-error-parse + correct-name-and-type gates that language's wiring; a failing language keeps its `file_type.rs` mapping (text fallback) with its structured touchpoints reverted, without blocking the other.

## Critical files & anchors

- `crates/zg-core/src/extraction/code/extractor/walk.rs:136-195` — `classify_code_node` substring chain; the destructor `struct`-false-positive verified here drives the step-5 drop.
- `crates/zg-core/src/extraction/code/families/metadata.rs:70-135,206` — `extract_preceding_doc` break, `extract_common_modifiers` match + haystack rebuild source, private `push_unique` driving the step-2 local-dedup decision.
- `crates/zg-core/src/extraction/code/languages/java.rs` — adapter template for both languages.
- `crates/zg-core/src/extraction/code/extractor/entry.rs:28-57,91-114` — `has_grammar` + `language_for_format` + the `resolve_adapter/has_grammar/language` fallback gate.
- `crates/zg-core/src/file_type.rs:25-58` — `CODE_EXTENSIONS`.

## Verification

- `cargo test -p zg-core` green, including new file-type, membership, adapter-resolution, family-fix unit, VB-hook unit (incl. C++ `friend` negative), constructor-`New` unit, and per-format table-driven extraction tests asserting exact `(symbol_name, symbol_type)` for every listed kind.
- `cargo clippy -p zg-core --all-targets` clean under workspace deny lints; `cargo doc -p zg-core --no-deps` warning-free.
- End-to-end: temp tree with one `.cs` (record + property) and one `.vb` (class + `Sub`), each with a unique probe token (e.g. `DotNetProbeAlpha`); `cargo run -p zg -- index <dir>` then `query DotNetProbeAlpha <dir>` (check CLI arg order in `crates/zg/src/main.rs` first); hits in both formats; `.csproj` and `.fs` content still indexed as text.
- Per-language tie-down: step 5/6 acceptance gates that language's wiring; a failing language keeps its `file_type.rs` mapping (text fallback) with its structured touchpoints reverted, without blocking the other.

## Assumptions & contingencies

- Scope is structured C# + VB; project/Razor/XAML files and all F# files intentionally remain text-indexed.
- Dropped the v1 escape hatches (F# ABI incompatibility, VB error-proneness): against tree-sitter 0.25.10 both grammars parse idiomatic fixtures clean, so those guard non-risks. The real risk is semantic extraction quality, guarded by the table-driven name-and-type-asserting tests above.
- If a hypothesized node kind is absent from `NODE_TYPES`: drop that kind (keep-only-if-present rule); never guess kinds into the tables. Re-adding any dropped C# destructor/operator/event/indexer or VB value-like kind requires its name/type override plus fixture rows — a new plan, not a drive-by.

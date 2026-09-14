# Core

`zvec-grep-rs` is a high-performance Rust port of `zvec-grep`, providing hybrid workspace search (lexical grep + vector semantic search) designed for both humans and AI agents.

## Workspace Architecture

Cargo workspace consisting of three primary crates:
- `zg-core` (`crates/zg-core`): Core search engine, AST-based entity extraction (Tree-sitter), embedding pipelines, Zvec vector storage, lexical search, indexing state coordination, and hybrid retrieval fusion.
- `zg-server` (`crates/zg-server`): Local HTTP daemon (Axum), MCP server (`rmcp`), authentication, and job scheduling.
- `zg` (`crates/zg`): CLI application (`clap`) orchestrating local search, daemon management, and MCP client/server commands.
- Local inference backends (`onnx`, `llama` features) are opt-in and off by default; gating rules: `mem:conventions`, versions: `mem:tech_stack`

## Key References
- Language, build system, and dependencies: `mem:tech_stack`
- Development, test, and system execution commands: `mem:suggested_commands`
- Workspace coding conventions, architectural invariants, and domain rules: `mem:conventions`
- Task verification, linting, and completion criteria: `mem:task_completion`

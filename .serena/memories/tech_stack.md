# Tech Stack

- Language: Rust 2024 edition (MSRV 1.88)
- Build / Package Manager: Cargo with resolver = "3"
- Storage & Vector Engine: `zvec-rust` (v0.7) for high-performance vector operations
- AST & Parsing: `tree-sitter` (v0.25) with language grammars (Rust, Python, TypeScript, JavaScript, Go, C, C++, Java, C# via `tree-sitter-c-sharp` 0.23, VB.NET via `tree-sitter-vb-dotnet` 0.1), `pulldown-cmark` (v0.13) for Markdown
- Embeddings & Tokenization: `tokenizers` (v0.21, onig/http features), `safetensors` (v0.6), `half` (v2); backends model2vec + qwen (always on), onnx (`ort` 2.0.0-rc, `onnx` feature) + llama-cpp (`llama-cpp-2`, `llama` feature, both off by default)
- Lexical Search: `grep-searcher`, `grep-matcher`, `grep-regex`, `regex`, `globset`, `ignore`, `walkdir`
- Concurrency & Async: `tokio` (v1, multi-thread), `rayon` (v1), `crossbeam-channel`
- HTTP & Networking: `axum` (v0.8), `tower`, `tower-http`, `reqwest` (v0.12), `ureq` (v3)
- MCP Server: `rmcp` (v0.6, requires `client` feature when `transport-child-process` is used)
- CLI & Diagnostics: `clap` (v4, derive/env), `tracing`, `tracing-subscriber`
- Serialization & Security: `serde`, `serde_json`, `sha2`, `hmac`, `uuid`

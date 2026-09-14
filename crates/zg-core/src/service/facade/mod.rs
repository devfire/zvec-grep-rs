//! `ZvecGrepService` facade: the sync engine entry point.
//!
//! Mirrors `../zvec-grep/src/engine/service/zvec-grep.ts` (`createZvecGrep`,
//! `ZvecGrepService`, `openWorkspaceReadSession`). Every method delegates to
//! the existing [`WorkspaceIndex`](crate::service::workspace_index::WorkspaceIndex),
//! [`pipeline::indexing`](crate::pipeline::indexing), and
//! [`pipeline::search`](crate::pipeline::search) pieces; the facade owns no
//! timers and no caches.
//!
//! Divergence notes (see `docs/ts-divergence.md`): the facade is sync —
//! [`EmbeddingModel::embed`](crate::models::EmbeddingModel::embed) is sync,
//! so no tokio dependency is needed here and the async wrapping happens in
//! the daemon (phase G) via `spawn_blocking`. Read sessions are explicit
//! RAII guards ([`ReadSession`]); idle-TTL eviction
//! belongs to the daemon, the only layer with a runtime. The method is named
//! [`context`](ZvecGrepService::context) after the TS method and the
//! `ZvecGrepContext*` DTOs, not `search`.
//!
//! Layout: `ZvecGrepService` lives in `service`; indexing
//! (`ensure_index`, `drop_index`) in `index`, model resolution in
//! `models`, status/info in `info`, hybrid search in `context`,
//! read sessions in `session`, abort plumbing in `signal`.

mod context;
mod index;
mod info;
mod models;
mod service;
mod session;
mod signal;

pub use context::{DEFAULT_CONTEXT_LIMIT, DEFAULT_CONTEXT_TOTAL_LIMIT};
pub use models::DEFAULT_EMBEDDING_REFERENCE;
pub use service::{CreateZvecGrepOptions, ZvecGrepService, create_zvec_grep};
pub use session::ReadSession;

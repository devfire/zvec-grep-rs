//! Daemon backend: one actor task per root over a shared scheduler and pool.
//!
//! Mirrors `../zvec-grep/src/daemon/backend.ts` (`DaemonBackend`: search /
//! index / index-drop / rg / index-status / server-status). The TS class
//! holds thirteen fields of plain `Map`s mutated across `await` points —
//! safe only on the single-threaded event loop. The Rust shape is
//! deliberately different (see `docs/ts-divergence.md`):
//!
//! - Each root is owned by one actor task holding `RootRuntime`, its
//!   `IndexCoordinator`, its `WatchManager`, and its read-session cache as
//!   plain `&mut` state. `DaemonBackend` holds only the
//!   [`RuntimeManager`](crate::runtime_manager::RuntimeManager)
//!   (key → sender + join handle).
//! - Promise-chain generation indexing becomes sequential message
//!   processing; [`Generation`](crate::root_runtime::Generation) survives
//!   as a staleness newtype, not a concurrency mechanism.
//! - `droppingRoots` / `shuttingDown` are control-flow states, and
//!   `closePromise` is [`RuntimeManager::close`](crate::runtime_manager::RuntimeManager::close)
//!   awaiting actor joins plus
//!   [`JobScheduler::close`](crate::job_scheduler::JobScheduler::close)
//!   awaiting in-flight blocking work (M6).
//! - Searches during an index read the last committed session instead of
//!   the TS writer context (documented staleness trade-off).
//!
//! Layout: `facade::DaemonBackend` is the command surface (`facade`);
//! per-root work lives in `root_actor` (message loop, index/status/rg/drop)
//! and `search` (freshness + cached search); scheduler runs in `index_run`;
//! spawning in `runtime`; the actor protocol in `actor`; shared config in
//! `config`; failure types in `error`; request/response shapes in
//! `search_types` and `request_types`; `util` holds the small helpers.

mod actor;
mod config;
mod error;
mod facade;
mod index_run;
mod request_types;
mod root_actor;
mod runtime;
mod search;
mod search_types;
#[cfg(test)]
mod tests;
mod util;

pub use actor::RootHandle;
pub use config::{BackendShared, DaemonBackendOptions, ServiceConfig};
pub use error::BackendError;
pub use facade::DaemonBackend;
pub use request_types::{DaemonIndexStatus, DaemonServerStatus, IndexInput, RgQuery};
pub use search_types::{
    BackgroundIndexState, DaemonSearchResult, ResultFreshness, SearchFreshness, SearchIndexing,
    SearchQuery, SearchRoute, SearchRouteMode,
};

pub(crate) use actor::RootCommand;
pub(crate) use runtime::spawn_root_actor;

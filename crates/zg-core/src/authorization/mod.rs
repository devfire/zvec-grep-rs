//! Remote-embedding authorization policy: grant store, permits, and
//! planning. Mirrors `src/authorization/` in TypeScript as a peer of the
//! daemon, not inside it, so `zg --mode direct` enforces the same permit
//! guard as the daemon without linking the server stack.

pub mod error;
pub mod operation;
pub mod planner;
pub mod prompt;
pub mod store;
pub mod target;
pub mod types;

pub use error::{AuthError, RemoteEmbeddingPurpose};
pub use operation::{
    RemoteEmbeddingAuthorizationManager, RemoteEmbeddingGuard,
    create_remote_embedding_operation_permit, with_remote_embedding_operation_permit,
};
pub use planner::{PlanIndexInput, PlanSearchInput, plan_remote_index_authorization};
pub use planner::{index_status_is_fresh, plan_remote_search_authorization};
pub use prompt::{
    REMOTE_EMBEDDING_ELICITATION_UNSUPPORTED_MESSAGE, RemoteEmbeddingPromptInput,
    format_remote_embedding_authorization_prompt, remote_embedding_disclosure_data,
};
pub use store::{
    DOCUMENT_VERSION, GRANT_FILE, RemoteEmbeddingAuthorizationStore, SIGNING_KEY_ENV_VAR,
    SIGNING_KEY_FILE,
};
pub use target::{
    canonicalize_workspace_roots, create_remote_embedding_request, create_remote_embedding_target,
    remote_embedding_target_fingerprint, workspace_fingerprint,
};
pub use types::{
    AuthorizationStatus, ContentKind, GrantStatus, PlanReason, REMOTE_EMBEDDING_CAPABILITY,
    RemoteEmbeddingDisclosure, RemoteEmbeddingDocument, RemoteEmbeddingGrant,
    RemoteEmbeddingOperation, RemoteEmbeddingPermit, RemoteEmbeddingPlan, RemoteEmbeddingRequest,
    RemoteEmbeddingScope, RemoteEmbeddingTarget, TargetFingerprint, WorkspaceContentDisclosure,
    WorkspaceFingerprint,
};

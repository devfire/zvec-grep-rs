//! Operation permits and the remote-embedding guard, mirroring
//! `src/authorization/operation.ts`.
//!
//! Divergence: TS scopes the permit with `AsyncLocalStorage` (async task
//! context); the sync engine uses a thread-local cell instead. The daemon
//! (phase G) sets the permit around each `spawn_blocking` embed call, so
//! the permit never leaks across operations on a shared thread.

use std::cell::RefCell;

use super::error::AuthError;
use super::store::RemoteEmbeddingAuthorizationStore;
use super::types::{
    REMOTE_EMBEDDING_CAPABILITY, RemoteEmbeddingPermit, RemoteEmbeddingRequest,
    RemoteEmbeddingScope, RemoteEmbeddingTarget,
};
use crate::error::EngineResult;
use crate::types::UnixMillis;

thread_local! {
    static CURRENT_PERMIT: RefCell<Option<RemoteEmbeddingPermit>> = const { RefCell::new(None) };
}

/// Issues a permit for the target at the given scope.
#[must_use]
pub fn create_remote_embedding_operation_permit(
    target: RemoteEmbeddingTarget,
    scope: RemoteEmbeddingScope,
) -> RemoteEmbeddingPermit {
    RemoteEmbeddingPermit {
        capability: REMOTE_EMBEDDING_CAPABILITY,
        scope,
        target,
        issued_at: UnixMillis::now(),
        operation_id: uuid::Uuid::new_v4().to_string(),
    }
}

/// Runs `operation` with `permit` as the ambient permit. `None` runs
/// unscoped, so the guard fails closed inside.
pub fn with_remote_embedding_operation_permit<T>(
    permit: Option<RemoteEmbeddingPermit>,
    operation: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<RemoteEmbeddingPermit>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT_PERMIT.with(|cell| *cell.borrow_mut() = self.0.take());
        }
    }
    let previous = CURRENT_PERMIT.with(|cell| std::mem::replace(&mut *cell.borrow_mut(), permit));
    let _restore = Restore(previous);
    operation()
}

/// Returns the ambient permit, if any (primarily for tests).
#[cfg(test)]
fn current_permit() -> Option<RemoteEmbeddingPermit> {
    CURRENT_PERMIT.with(|cell| cell.borrow().clone())
}

/// Guard over remote-embedding requests: fails closed without an ambient
/// permit bound to the same provider/model/endpoint, and re-checks the
/// workspace grant on every call so revocation takes effect immediately.
pub struct RemoteEmbeddingGuard {
    store: RemoteEmbeddingAuthorizationStore,
}

impl Default for RemoteEmbeddingGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl RemoteEmbeddingGuard {
    /// Guards with the default grant store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            store: RemoteEmbeddingAuthorizationStore::new(),
        }
    }

    /// Guards with an explicit store (tests, daemon wiring).
    #[must_use]
    pub fn with_store(store: RemoteEmbeddingAuthorizationStore) -> Self {
        Self { store }
    }

    /// Checks one request against the ambient permit.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::AuthorizationRequired`] when no ambient permit covers the request or the workspace grant is missing or revoked, or [`AuthError::StoreFailed`] when the grant store cannot be read.
    pub fn check(&self, request: &RemoteEmbeddingRequest) -> EngineResult<()> {
        let permit = CURRENT_PERMIT.with(|cell| cell.borrow().clone());
        let Some(permit) = permit else {
            return Err(required(request, None));
        };
        if permit.capability != REMOTE_EMBEDDING_CAPABILITY
            || permit.target.provider != request.provider
            || permit.target.model != request.model
            || permit.target.endpoint != request.endpoint
        {
            return Err(required(request, None));
        }
        if permit.scope == RemoteEmbeddingScope::Workspace
            && !self.store.has_grant(&permit.target)?
        {
            return Err(required(
                request,
                Some("Workspace grant is missing, invalid, or revoked."),
            ));
        }
        Ok(())
    }
}

fn required(request: &RemoteEmbeddingRequest, detail: Option<&str>) -> crate::error::EngineError {
    crate::error::EngineError::from(AuthError::AuthorizationRequired {
        provider: request.provider.clone(),
        model: request.model.clone(),
        endpoint: request.endpoint.clone(),
        purpose: request.purpose,
        detail: detail.map(str::to_owned),
    })
}

/// Authorization manager: issues permits, persisting workspace grants.
/// Mirrors `RemoteEmbeddingAuthorizationManager`.
#[derive(Debug, Default)]
pub struct RemoteEmbeddingAuthorizationManager {
    store: RemoteEmbeddingAuthorizationStore,
}

impl RemoteEmbeddingAuthorizationManager {
    /// Manages permits against the default grant store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            store: RemoteEmbeddingAuthorizationStore::new(),
        }
    }

    /// Manages permits against an explicit store (tests, daemon wiring).
    #[must_use]
    pub fn with_store(store: RemoteEmbeddingAuthorizationStore) -> Self {
        Self { store }
    }

    /// Returns a workspace permit when a valid grant already covers the
    /// target, `None` otherwise.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::StoreFailed`] when the grant store cannot be read.
    pub fn existing_workspace_permit(
        &self,
        target: &RemoteEmbeddingTarget,
    ) -> EngineResult<Option<RemoteEmbeddingPermit>> {
        if !self.store.has_grant(target)? {
            return Ok(None);
        }
        Ok(Some(create_remote_embedding_operation_permit(
            target.clone(),
            RemoteEmbeddingScope::Workspace,
        )))
    }

    /// Issues a permit, persisting the grant for workspace scope.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::StoreFailed`] when the signing key or grant file cannot be written.
    pub fn grant(
        &self,
        target: &RemoteEmbeddingTarget,
        scope: RemoteEmbeddingScope,
    ) -> EngineResult<RemoteEmbeddingPermit> {
        if scope == RemoteEmbeddingScope::Workspace {
            self.store.grant(target)?;
        }
        Ok(create_remote_embedding_operation_permit(
            target.clone(),
            scope,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorization::error::RemoteEmbeddingPurpose;
    use crate::authorization::target::create_remote_embedding_target;
    use crate::authorization::types::ContentKind;

    fn request() -> RemoteEmbeddingRequest {
        RemoteEmbeddingRequest {
            provider: "qwen".to_owned(),
            model: "text-embedding-v4".to_owned(),
            endpoint: "https://example.invalid/embeddings".to_owned(),
            purpose: RemoteEmbeddingPurpose::Query,
            content_kinds: vec![ContentKind::Text],
            content_count: 1,
        }
    }

    fn target_in(dir: &std::path::Path) -> RemoteEmbeddingTarget {
        let root = dir.join("repo");
        std::fs::create_dir_all(&root).expect("mkdir");
        create_remote_embedding_target(
            &[root.to_string_lossy().into_owned()],
            "qwen",
            "text-embedding-v4",
            "https://example.invalid/embeddings",
        )
        .expect("target")
    }

    #[test]
    fn guard_fails_closed_without_permit() {
        let guard = RemoteEmbeddingGuard::new();
        let error = guard.check(&request()).expect_err("no permit");
        assert_eq!(
            error.code().to_string(),
            "ZVEC_GREP.ENGINE.AUTH.REMOTE_EMBEDDING_REQUIRED"
        );
    }
    #[test]
    fn once_permit_needs_no_stored_grant() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = target_in(dir.path());
        let keys = || RemoteEmbeddingAuthorizationStore::with_signing_key(dir.path().join("k"));
        let manager = RemoteEmbeddingAuthorizationManager::with_store(keys());
        let permit = manager
            .grant(&target, RemoteEmbeddingScope::Once)
            .expect("once");
        let scoped = RemoteEmbeddingGuard::with_store(keys());
        with_remote_embedding_operation_permit(Some(permit), || {
            scoped.check(&request()).expect("once permit passes");
        });
        assert!(current_permit().is_none());
    }

    #[test]
    fn workspace_permit_rechecks_revocation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = target_in(dir.path());
        let keys =
            || RemoteEmbeddingAuthorizationStore::with_signing_key(dir.path().join("signing.key"));
        let manager = RemoteEmbeddingAuthorizationManager::with_store(keys());
        assert!(
            manager
                .existing_workspace_permit(&target)
                .expect("check")
                .is_none()
        );
        let permit = manager
            .grant(&target, RemoteEmbeddingScope::Workspace)
            .expect("grant");
        assert!(
            manager
                .existing_workspace_permit(&target)
                .expect("check")
                .is_some()
        );
        let scoped = RemoteEmbeddingGuard::with_store(keys());
        with_remote_embedding_operation_permit(Some(permit.clone()), || {
            scoped.check(&request()).expect("granted passes");
        });
        keys().revoke(&target).expect("revoke");
        with_remote_embedding_operation_permit(Some(permit), || {
            let error = scoped.check(&request()).expect_err("revoked");
            assert_eq!(
                error.code().to_string(),
                "ZVEC_GREP.ENGINE.AUTH.REMOTE_EMBEDDING_REQUIRED"
            );
            assert!(error.context().unwrap_or_default().contains("revoked"));
        });
    }

    #[test]
    fn mismatched_permit_fails_closed() {
        use crate::authorization::types::{TargetFingerprint, WorkspaceFingerprint};
        let target = RemoteEmbeddingTarget {
            workspace_roots: vec!["/repo".to_owned()],
            workspace_fingerprint: WorkspaceFingerprint::from_hex("w".to_owned()),
            provider: "qwen".to_owned(),
            model: "other-model".to_owned(),
            endpoint: "https://example.invalid/embeddings".to_owned(),
            target_fingerprint: TargetFingerprint::from_hex("t".to_owned()),
        };
        let permit = create_remote_embedding_operation_permit(target, RemoteEmbeddingScope::Once);
        let guard = RemoteEmbeddingGuard::new();
        with_remote_embedding_operation_permit(Some(permit), || {
            guard.check(&request()).expect_err("model mismatch");
        });
    }
}

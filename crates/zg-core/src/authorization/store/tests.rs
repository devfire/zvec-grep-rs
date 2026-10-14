use std::fs;
use std::path::{Path, PathBuf};

use super::crypto::stable_stringify;
use super::{GRANT_FILE, RemoteEmbeddingAuthorizationStore, SIGNING_KEY_FILE};
use crate::authorization::target::{canonicalize_workspace_roots, create_remote_embedding_target};
use crate::authorization::types::{
    RemoteEmbeddingGrant, RemoteEmbeddingScope, RemoteEmbeddingTarget,
};
use crate::paths::STATE_DIR_NAME;

/// Canonical on-disk root: grant and key files live under the
/// canonicalized path, so file assertions must join from there rather
/// than the raw (possibly symlinked) input.
fn canonical_path(root: &Path) -> PathBuf {
    let roots = canonicalize_workspace_roots(&[root.to_string_lossy().into_owned()]);
    PathBuf::from(roots.into_iter().next().expect("canonical root"))
}

fn store_in(dir: &Path) -> RemoteEmbeddingAuthorizationStore {
    RemoteEmbeddingAuthorizationStore::with_signing_key(dir.join("signing.key"))
}

fn target_for(root: &Path) -> RemoteEmbeddingTarget {
    create_remote_embedding_target(
        &[root.to_string_lossy().into_owned()],
        "qwen",
        "text-embedding-v4",
        "https://example.invalid/embeddings",
    )
    .expect("target")
}

#[test]
fn stable_stringify_sorts_keys_like_ts() {
    let value = serde_json::json!({
        "version": 1,
        "scope": "workspace",
        "workspaceRoots": ["/b", "/a"],
        "id": "x",
    });
    assert_eq!(
        stable_stringify(&value),
        r#"{"id":"x","scope":"workspace","version":1,"workspaceRoots":["/b","/a"]}"#
    );
}

#[test]
fn grants_are_signed_target_bound_and_revocable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root_a = dir.path().join("a");
    let root_b = dir.path().join("b");
    fs::create_dir_all(&root_a).expect("mkdir a");
    fs::create_dir_all(&root_b).expect("mkdir b");
    let store = store_in(dir.path());
    let target = create_remote_embedding_target(
        &[
            root_a.to_string_lossy().into_owned(),
            root_b.to_string_lossy().into_owned(),
        ],
        "qwen",
        "text-embedding-v4",
        "https://example.invalid/embeddings",
    )
    .expect("target");

    store.grant(&target).expect("grant");
    assert!(store.has_grant(&target).expect("has_grant"));
    let status = store.status(&root_a.to_string_lossy()).expect("status");
    assert_eq!(status.grants.len(), 1);
    assert!(status.grants[0].valid);
    assert_eq!(status.grants[0].scope, RemoteEmbeddingScope::Workspace);

    // Tampering with a signed field invalidates the grant.
    let text = fs::read_to_string(&status.path).expect("read grants");
    let tampered = text.replace(
        "https://example.invalid/embeddings",
        "https://tampered.invalid/embeddings",
    );
    assert_ne!(text, tampered);
    fs::write(&status.path, tampered).expect("tamper");
    assert!(!store.has_grant(&target).expect("has_grant after tamper"));

    // Re-grant, then revoke-all from the sibling root clears only that
    // file: the disjoint sibling copy survives on disk but no longer
    // authorizes, because every canonical root must hold a valid grant.
    store.grant(&target).expect("re-grant");
    assert_eq!(
        store
            .revoke_all(&root_b.to_string_lossy())
            .expect("revoke_all"),
        1
    );
    let status_a = store.status(&root_a.to_string_lossy()).expect("status a");
    assert_eq!(status_a.grants.len(), 1);
    assert!(!store.has_grant(&target).expect("has_grant after revoke"));
    // Revoking the target clears the stale sibling copy; afterwards
    // there is nothing left to revoke.
    assert!(store.revoke(&target).expect("revoke stale"));
    assert!(!store.has_grant(&target).expect("has_grant after revoke"));
    assert!(!store.revoke(&target).expect("revoke missing"));
}
#[test]
fn truncated_signature_is_invalid() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("repo");
    fs::create_dir_all(&root).expect("mkdir");
    let store = store_in(dir.path());
    let target = target_for(&root);
    let grant = store.grant(&target).expect("grant");
    let keys = store
        .candidate_keys(&root.to_string_lossy())
        .expect("read keys");
    // Derived per-root key first, raw global secret second (legacy grants).
    assert_eq!(keys.len(), 2);
    let key = keys.into_iter().next().expect("key present");
    assert!(RemoteEmbeddingAuthorizationStore::verify_grant(
        &grant, &key
    ));
    // Length mismatch fails before content comparison.
    let mut short = grant.clone();
    short.signature.truncate(short.signature.len() / 2);
    assert!(!RemoteEmbeddingAuthorizationStore::verify_grant(
        &short, &key
    ));
}

#[test]
fn empty_store_has_no_grants() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("repo");
    fs::create_dir_all(&root).expect("mkdir");
    let store = store_in(dir.path());
    let target = target_for(&root);
    assert!(!store.has_grant(&target).expect("has_grant"));
    let status = store.status(&root.to_string_lossy()).expect("status");
    assert!(status.grants.is_empty());
    assert_eq!(
        store
            .revoke_all(&root.to_string_lossy())
            .expect("revoke_all"),
        0
    );
}

#[test]
fn concurrent_key_creation_converges_on_persisted_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(SIGNING_KEY_FILE);
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let path = path.clone();
            std::thread::spawn(move || {
                RemoteEmbeddingAuthorizationStore::get_or_create_key_file(&path)
                    .expect("create key")
            })
        })
        .collect();
    let keys: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("join creator"))
        .collect();
    // Every creator returns the one persisted key: a loser that
    // overwrote (or kept its own unpublished key) would diverge.
    let persisted = RemoteEmbeddingAuthorizationStore::read_key_file(&path)
        .expect("read persisted")
        .expect("key persisted");
    assert!(keys.iter().all(|key| key == &persisted));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&path)
            .expect("key metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // No temp files are left behind.
    let leftovers: Vec<_> = fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name() != SIGNING_KEY_FILE)
        .collect();
    assert!(leftovers.is_empty(), "leftovers: {leftovers:?}");
}

#[test]
fn derived_keys_reject_cross_workspace_replay() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root_a = dir.path().join("a");
    let root_b = dir.path().join("b");
    fs::create_dir_all(&root_a).expect("mkdir a");
    fs::create_dir_all(&root_b).expect("mkdir b");
    // Global secret lives outside both workspaces.
    let global_key = dir.path().join("global.key");
    let store = RemoteEmbeddingAuthorizationStore::with_signing_key(global_key.clone());
    let target_a = target_for(&root_a);
    store.grant(&target_a).expect("grant a");
    assert!(store.has_grant(&target_a).expect("has_grant a"));
    // Granting mints only the global key file, never a workspace-local key.
    let canonical_a = canonical_path(&root_a);
    let canonical_b = canonical_path(&root_b);
    let key_a = canonical_a.join(STATE_DIR_NAME).join(SIGNING_KEY_FILE);
    assert!(!key_a.exists());
    assert!(global_key.is_file());
    let key_a = global_key;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&key_a)
            .expect("key metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // Replaying A's grant file into B does not verify under B's derived key.
    let file_a = canonical_a.join(STATE_DIR_NAME).join(GRANT_FILE);
    let file_b = canonical_b.join(STATE_DIR_NAME).join(GRANT_FILE);
    if let Some(parent) = file_b.parent() {
        fs::create_dir_all(parent).expect("mkdir b authz");
    }
    fs::copy(&file_a, &file_b).expect("replay grant file");
    let status_b = store.status(&root_b.to_string_lossy()).expect("status b");
    assert_eq!(status_b.grants.len(), 1);
    assert!(!status_b.grants[0].valid);
    assert!(!store.has_grant(&target_for(&root_b)).expect("has_grant b"));
}

#[test]
fn revoke_all_confines_sibling_fanout_to_boundary() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root_a = dir.path().join("a");
    let nested = root_a.join("sub");
    let outside = dir.path().join("outside");
    fs::create_dir_all(&nested).expect("mkdir nested");
    fs::create_dir_all(&outside).expect("mkdir outside");
    let store = store_in(dir.path());
    // One target spanning the boundary root, a nested root, and a
    // disjoint root: grant fans out to all three files.
    let target = create_remote_embedding_target(
        &[
            root_a.to_string_lossy().into_owned(),
            nested.to_string_lossy().into_owned(),
            outside.to_string_lossy().into_owned(),
        ],
        "qwen",
        "text-embedding-v4",
        "https://example.invalid/embeddings",
    )
    .expect("target");
    store.grant(&target).expect("grant");
    assert_eq!(
        store
            .revoke_all(&root_a.to_string_lossy())
            .expect("revoke_all"),
        1
    );
    // Nested sibling inside the boundary is cleaned; the disjoint
    // sibling outside it is skipped and keeps its grant.
    let nested_status = store
        .status(&nested.to_string_lossy())
        .expect("nested status");
    assert!(nested_status.grants.is_empty());
    let outside_status = store
        .status(&outside.to_string_lossy())
        .expect("outside status");
    assert_eq!(outside_status.grants.len(), 1);
    // Clearing the boundary root still deauthorizes the whole target.
    assert!(!store.has_grant(&target).expect("has_grant after revoke"));
}

#[test]
fn revoke_all_on_missing_root_returns_zero_without_creating_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store_in(dir.path());
    let missing = dir.path().join("does-not-exist");
    assert_eq!(
        store
            .revoke_all(&missing.to_string_lossy())
            .expect("revoke_all missing"),
        0
    );
    assert!(!missing.exists());
    assert!(!store.revoke(&target_for(&missing)).expect("revoke missing"));
    assert!(!missing.exists());
}

#[test]
fn workspace_local_signing_key_cannot_forge_grant() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("repo");
    fs::create_dir_all(&root).expect("mkdir");
    let target = target_for(&root);
    let canonical = canonical_path(&root);
    let state_dir = canonical.join(STATE_DIR_NAME);
    fs::create_dir_all(&state_dir).expect("mkdir state");
    // Attacker ships a signing key inside the workspace plus a grant
    // signed with it (both travel with a clone). The victim's global
    // secret lives outside the workspace and is unrelated to that key.
    let attacker_key = [0xABu8; 32];
    fs::write(
        state_dir.join(SIGNING_KEY_FILE),
        crate::utils::hash::to_hex(&attacker_key),
    )
    .expect("plant workspace key");
    let unsigned = RemoteEmbeddingGrant {
        version: super::DOCUMENT_VERSION,
        id: "forged".to_owned(),
        capability: crate::authorization::types::REMOTE_EMBEDDING_CAPABILITY.to_owned(),
        scope: RemoteEmbeddingScope::Workspace,
        workspace_roots: vec![canonical.to_string_lossy().into_owned()],
        workspace_fingerprint: target.workspace_fingerprint.to_string(),
        provider: target.provider.clone(),
        model: target.model.clone(),
        endpoint: target.endpoint.clone(),
        target_fingerprint: target.target_fingerprint.to_string(),
        granted_at: 0,
        signature: String::new(),
    };
    let signature = super::crypto::sign(&super::crypto::unsigned_grant(&unsigned), &attacker_key)
        .expect("sign forged grant");
    let forged = RemoteEmbeddingGrant {
        signature,
        ..unsigned
    };
    let document = serde_json::json!({ "version": 1, "grants": [forged] });
    fs::write(state_dir.join(GRANT_FILE), document.to_string()).expect("plant forged grant");
    // Victim has no global secret yet, then creates a legitimate one by
    // granting an unrelated root: neither state may accept the forgery.
    let victim = store_in(dir.path());
    assert!(!victim.has_grant(&target).expect("has_grant without global"));
    let other = dir.path().join("other");
    fs::create_dir_all(&other).expect("mkdir other");
    victim.grant(&target_for(&other)).expect("legit grant");
    assert!(!victim.has_grant(&target).expect("has_grant with global"));
    let status = victim.status(&root.to_string_lossy()).expect("status");
    assert_eq!(status.grants.len(), 1);
    assert!(!status.grants[0].valid);
}

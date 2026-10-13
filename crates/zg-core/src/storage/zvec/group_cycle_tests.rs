//! Regression tests for cyclic fragment `group` links (issue #88).
//!
//! The validated write path cannot create a cycle, so these tests encode
//! the cyclic documents directly with `fragment_to_doc` — the state a
//! foreign or corrupt `index.zvec` can present on disk. Both group-chain
//! walks must omit the malformed chain instead of looping forever.

use super::*;

use crate::ids::EntityId;
use crate::types::{
    Content, Entity, EntityFragment, FileFormat, FileKind, Range, SearchMetric,
    WorkspaceIndexEmbeddingSchema,
};

const ID_A: &str = "entity-a";
const ID_B: &str = "entity-b";
const ID_C: &str = "entity-c";
const ID_D: &str = "entity-d";

fn fragment(file: &FileInfo, id: &str, group: Option<&str>, text: &str) -> EntityFragment {
    EntityFragment {
        entity: Entity {
            id: EntityId::from_raw(id.to_owned()),
            file_id: file.id.clone(),
            range: Range::Text {
                start_line: 0,
                end_line: 0,
                start_offset: 0,
                end_offset: text.len(),
            },
            content: Content::Text {
                text: text.to_owned(),
            },
            metadata: None,
        },
        group: group.map(ToOwned::to_owned),
    }
}

struct Fixture {
    storage: ZvecWorkspaceIndexStorage,
    files: HashMap<String, FileRecord>,
    // Last: fields drop in declaration order, so the collection closes
    // before the temp directory is removed.
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    initialize_zvec().expect("zvec init");
    let dir = tempfile::TempDir::new().expect("tempdir");
    let schema = WorkspaceIndexEmbeddingSchema {
        provider: "test".to_owned(),
        model: "dummy".to_owned(),
        dimension: 4,
        metric: SearchMetric::Cosine,
    };
    let collection_path = dir.path().join("index.zvec");
    let collection = Collection::create_and_open(
        &collection_path.to_string_lossy(),
        &create_entities_schema(&schema).expect("entities schema"),
        None,
    )
    .expect("create collection");
    let file = FileInfo {
        id: FileId::from_raw("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned()),
        absolute_path: dir.path().join("a.txt").to_string_lossy().into_owned(),
        relative_path: "a.txt".to_owned(),
        root_path: dir.path().to_string_lossy().into_owned(),
        size_bytes: 16,
        last_modified_time: UnixMillis::from_millis(1_700_000_000_000),
        content_hash: Some("hash-a".to_owned()),
        kind: FileKind::Text,
        format: FileFormat::parse("text"),
        index_status: None,
    };
    // A <-> B is the cycle; C is a major (group == own id) and D -> C is a
    // valid one-hop chain that must keep resolving.
    let fragments = [
        fragment(&file, ID_A, Some(ID_B), "alpha"),
        fragment(&file, ID_B, Some(ID_A), "beta"),
        fragment(&file, ID_C, Some(ID_C), "gamma"),
        fragment(&file, ID_D, Some(ID_C), "delta"),
    ];
    let docs: Vec<Doc> = fragments
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let index = i32::try_from(index).expect("small fragment index");
            fragment_to_doc(&file, item, &[0.25, 0.5, 0.75, 1.0], index).expect("encode fragment")
        })
        .collect();
    let refs: Vec<&Doc> = docs.iter().collect();
    collection.upsert(&refs).expect("upsert cyclic docs");
    let record = FileRecord {
        info: file.clone(),
        entity_ids: vec![
            ID_A.to_owned(),
            ID_B.to_owned(),
            ID_C.to_owned(),
            ID_D.to_owned(),
        ],
    };
    let mut files = HashMap::new();
    files.insert(file.id.as_str().to_owned(), record.clone());
    let mut meta = FileMetaStore::open(&dir.path().join("files.json"), false).expect("meta store");
    meta.upsert(record).expect("meta upsert");
    let guard = acquire_read_write_lock(
        &dir.path().join("LOCK"),
        LockMode::Write,
        &LockOptions::new("groupCycleTest"),
    )
    .expect("storage lock");
    let storage = ZvecWorkspaceIndexStorage {
        read_only: false,
        collection: Some(collection),
        meta,
        file_ids_by_path: HashMap::new(),
        needs_optimize: false,
        _lock: guard,
    };
    Fixture {
        storage,
        files,
        _dir: dir,
    }
}

#[test]
fn cyclic_group_stored_entity_returns_none_and_valid_chain_resolves() {
    let fixture = fixture();
    for id in [ID_A, ID_B] {
        let entity = fixture
            .storage
            .stored_entity(id, "getEntity", &fixture.files)
            .expect("stored_entity must not hang or error on a cycle");
        assert!(entity.is_none(), "cyclic entity {id} must be omitted");
    }
    let resolved = fixture
        .storage
        .stored_entity(ID_D, "getEntity", &fixture.files)
        .expect("valid chain resolves")
        .expect("valid chain yields the major entity");
    assert_eq!(resolved.entity.id.as_str(), ID_C);
}

#[test]
fn cyclic_group_resolve_group_chains_omits_cycle_and_keeps_valid_chain() {
    let fixture = fixture();
    let collection = fixture.storage.collection.as_ref().expect("collection");
    let ids = [ID_A, ID_B, ID_C, ID_D];
    let docs = collection
        .fetch_with_options(&ids, None, false)
        .expect("fetch docs");
    let mut by_pk: HashMap<&str, &Doc> = HashMap::new();
    for doc in &docs {
        let pk = doc.get_pk().unwrap_or_default();
        if !pk.is_empty() {
            by_pk.entry(pk).or_insert(doc);
        }
    }
    let mut stored: Vec<Option<StoredFragment>> = ids
        .iter()
        .map(|id| {
            by_pk
                .get(id)
                .and_then(|doc| doc_to_stored_fragment(doc, &fixture.files).ok().flatten())
        })
        .collect();
    ZvecWorkspaceIndexStorage::resolve_group_chains(collection, &fixture.files, docs, &mut stored);
    assert!(
        stored.first().is_some_and(Option::is_none),
        "cyclic A must be omitted"
    );
    assert!(
        stored.get(1).is_some_and(Option::is_none),
        "cyclic B must be omitted"
    );
    let major = stored
        .get(2)
        .and_then(Option::as_ref)
        .expect("major C kept");
    assert_eq!(major.fragment.entity.id.as_str(), ID_C);
    let resolved = stored
        .get(3)
        .and_then(Option::as_ref)
        .expect("valid D -> C chain kept");
    assert_eq!(resolved.fragment.entity.id.as_str(), ID_C);
}

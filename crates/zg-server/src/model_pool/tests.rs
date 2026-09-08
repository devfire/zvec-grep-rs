//! Pool behavior: caching, shared failures, single-flight loads, LRU trim,
//! and close.

use std::sync::Arc;
use std::time::Duration;

use zg_core::error::EngineError;
use zg_core::models::EmbeddingModel;
use zg_core::models::catalog::ModelReference;
use zg_core::models::embeddings::CreateEmbeddingModelOptions;

use super::*;

fn stub_request() -> ModelLoadRequest {
    ModelLoadRequest {
        reference: ModelReference::new("stub/deterministic"),
        options: CreateEmbeddingModelOptions::default(),
    }
}

fn stub_pool(options: EmbeddingModelPoolOptions) -> EmbeddingModelPool {
    let mut options = options;
    options.create_model = Some(Arc::new(|_: &ModelLoadRequest| {
        Ok(Arc::new(zg_core::models::stub::StubEmbeddingModel::new(16)) as Arc<dyn EmbeddingModel>)
    }));
    EmbeddingModelPool::new(options)
}

#[tokio::test]
async fn acquire_caches_and_counts_leases() {
    let pool = stub_pool(EmbeddingModelPoolOptions::default());
    let first = pool.acquire(&stub_request()).await.unwrap();
    let second = pool.acquire(&stub_request()).await.unwrap();
    assert_eq!(
        pool.snapshot(),
        ModelPoolSnapshot {
            loaded: 1,
            active_leases: 2
        }
    );
    first.release();
    assert_eq!(pool.snapshot().active_leases, 1);
    drop(second);
    assert_eq!(pool.snapshot().active_leases, 0);
}

#[tokio::test]
async fn load_failure_is_shared_not_cached() {
    let pool = EmbeddingModelPool::new(EmbeddingModelPoolOptions {
        create_model: Some(Arc::new(|request: &ModelLoadRequest| {
            Err(EngineError::from(
                zg_core::models::error::ModelError::CatalogModelNotFound {
                    reference: request.reference.as_str().to_owned(),
                },
            ))
        })),
        ..EmbeddingModelPoolOptions::default()
    });
    let error = pool.acquire(&stub_request()).await.unwrap_err();
    assert_eq!(error.code(), "MODEL_LOAD_FAILED");
    assert!(matches!(error, AcquireError::Load { .. }));
    assert_eq!(pool.snapshot().loaded, 0);
}

#[tokio::test]
async fn concurrent_acquirers_share_one_load() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let loads = Arc::new(AtomicUsize::new(0));
    let loads_clone = loads.clone();
    let pool = EmbeddingModelPool::new(EmbeddingModelPoolOptions {
        create_model: Some(Arc::new(move |_: &ModelLoadRequest| {
            loads_clone.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(50));
            Ok(Arc::new(zg_core::models::stub::StubEmbeddingModel::new(8))
                as Arc<dyn EmbeddingModel>)
        })),
        ..EmbeddingModelPoolOptions::default()
    });
    let request = stub_request();
    let first = pool.acquire(&request);
    let second = pool.acquire(&request);
    let (first, second) = tokio::join!(first, second);
    first.unwrap();
    second.unwrap();
    assert_eq!(loads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn lru_trim_evicts_idle_models_beyond_cap() {
    let pool = stub_pool(EmbeddingModelPoolOptions {
        max_loaded_models: Some(1),
        idle_ttl: Some(Duration::from_secs(3600)),
        ..EmbeddingModelPoolOptions::default()
    });
    let first = pool.acquire(&stub_request()).await.unwrap();
    drop(first);
    assert_eq!(pool.snapshot().loaded, 1);
    let mut other = stub_request();
    other.reference = ModelReference::new("stub/other");
    let second = pool.acquire(&other).await.unwrap();
    assert_eq!(pool.snapshot().loaded, 1);
    drop(second);
}

#[tokio::test]
async fn close_evicts_idle_and_rejects_acquire() {
    let pool = stub_pool(EmbeddingModelPoolOptions::default());
    let lease = pool.acquire(&stub_request()).await.unwrap();
    pool.close().await;
    assert_eq!(lease.model().info().dimension, 16);
    drop(lease);
    assert_eq!(pool.snapshot().loaded, 0);
    assert!(matches!(
        pool.acquire(&stub_request()).await,
        Err(AcquireError::Closed)
    ));
}

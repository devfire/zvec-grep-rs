//! Transformers.js (ONNX) embedding backend, behind the `onnx` feature.
//!
//! Mirrors `src/engine/models/backends/transformers-js.ts`: the catalog's
//! pinned `repo`/`revision` supplies `tokenizer.json` plus one quantized
//! `onnx/*.onnx` artifact through the shared [`crate::models::download`]
//! cache, inference runs on `ort::Session`, and each text is prefixed by
//! purpose, truncated to `max_input_tokens`, pooled (`cls`/`mean`) and
//! L2-normalized exactly like the TypeScript pipeline options.
//!
//! Concurrency follows the C/D design note in `docs/ts-divergence.md`:
//! the `Arc<dyn EmbeddingModel>` owns `'static` weights (tokenizer plus a
//! session pool), and each `embed` leases a session from a bounded pool
//! that doubles as M6's gate. Sessions are created lazily up to
//! `min(8, available_parallelism)` and retained, mirroring the TypeScript
//! context-growth loop; a lease is one full batch, never one input.
//!
//! The `ort` dependency ships a CPU-only build, so a non-CPU
//! [`DeviceKind`] warns once through the load sink and falls back to CPU,
//! mirroring the TypeScript `usingCpuFallback` path.

use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, OnceLock};

use crate::error::{EngineError, EngineResult};
use crate::models::catalog::{PoolingKind, TransformersJsEntry};
use crate::models::download::{self, ModelDownloadReporter};
use crate::models::embeddings::{DeviceKind, EmbeddingResult, embed_validated, purpose_prefix};
use crate::models::error::ModelError;
use crate::models::{
    EmbeddingInput, EmbeddingInputKind, EmbeddingModel, EmbeddingModelInfo, EmbeddingPurpose,
    ModelLoadSink,
};
use crate::types::SearchMetric;

/// Upper bound on pooled ONNX sessions; mirrors
/// `DEFAULT_PARALLELISM_CAP` in the llama-cpp backend.
const MAX_SESSION_POOL_SIZE: usize = 8;

/// Remote ONNX filename selected by catalog dtype, mirroring the
/// transformers.js dtype-to-file mapping (`q4` → `model_q4.onnx`).
fn onnx_remote_file(entry: &TransformersJsEntry) -> &'static str {
    use crate::models::catalog::TransformersDtype;
    match entry.dtype {
        TransformersDtype::Q4 => "onnx/model_q4.onnx",
        TransformersDtype::Q8 => "onnx/model_quantized.onnx",
        TransformersDtype::Fp32 => "onnx/model.onnx",
    }
}
/// Builds the typed `TRANSFORMERS_JS_EMBED_FAILED` error for `entry`.
fn embed_failed(entry: &TransformersJsEntry, detail: impl std::fmt::Display) -> EngineError {
    EngineError::from(ModelError::TransformersJsEmbed {
        reference: entry.reference.to_owned(),
        repo: entry.repo.to_owned(),
        detail: detail.to_string(),
    })
}

/// HEAD probe for the optional external-data companion: only a 2xx answer
/// triggers the real (loud on failure) download; anything else means the
/// model is self-contained.
fn remote_file_exists(url: &str) -> bool {
    ureq::head(url)
        .call()
        .map(|response| (200..300).contains(&response.status().as_u16()))
        .unwrap_or(false)
}

/// Builds the typed `TRANSFORMERS_JS_TOKENIZATION_FAILED` error.
fn tokenize_failed(entry: &TransformersJsEntry, detail: impl std::fmt::Display) -> EngineError {
    EngineError::from(ModelError::TransformersJsTokenize {
        reference: entry.reference.to_owned(),
        repo: entry.repo.to_owned(),
        detail: detail.to_string(),
    })
}

/// Bounded pool of lazily-created ONNX sessions.
///
/// `acquire` pops an idle session, creates one while under `cap`, or
/// blocks on the condvar until a lease returns. Creation happens without
/// holding the lock; a creation failure decrements the reservation so a
/// later caller retries instead of wedging the pool.
struct SessionPool {
    state: Mutex<PoolState>,
    ready: Condvar,
    cap: usize,
    model_path: PathBuf,
    entry: TransformersJsEntry,
}

struct PoolState {
    idle: Vec<ort::session::Session>,
    /// Sessions created but not yet returned (idle + outstanding).
    created: usize,
}

/// One held session; returning it to the pool is the `Drop` impl, so a
/// panic or early return inside `embed` cannot leak pool capacity.
struct SessionLease<'a> {
    pool: &'a SessionPool,
    session: Option<ort::session::Session>,
}

impl Drop for SessionLease<'_> {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            let mut state = self
                .pool
                .state
                .lock()
                .unwrap_or_else(|err| err.into_inner());
            state.idle.push(session);
            self.pool.ready.notify_one();
        }
    }
}

impl SessionPool {
    fn new(entry: TransformersJsEntry, model_path: PathBuf) -> Self {
        let cap = std::thread::available_parallelism()
            .map(|cores| cores.get().clamp(1, MAX_SESSION_POOL_SIZE))
            .unwrap_or(1);
        Self {
            state: Mutex::new(PoolState {
                idle: Vec::new(),
                created: 0,
            }),
            ready: Condvar::new(),
            cap,
            model_path,
            entry,
        }
    }

    fn acquire(&self) -> EngineResult<SessionLease<'_>> {
        let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());
        loop {
            if let Some(session) = state.idle.pop() {
                return Ok(SessionLease {
                    pool: self,
                    session: Some(session),
                });
            }
            if state.created < self.cap {
                state.created = state.created.saturating_add(1);
                break;
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(|err| err.into_inner());
        }
        drop(state);
        match create_session(&self.model_path) {
            Ok(session) => Ok(SessionLease {
                pool: self,
                session: Some(session),
            }),
            Err(err) => {
                let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());
                state.created = state.created.saturating_sub(1);
                self.ready.notify_one();
                Err(embed_failed(&self.entry, err))
            }
        }
    }
}

/// Commits one CPU session from the cached model file.
fn create_session(model_path: &Path) -> Result<ort::session::Session, ort::Error> {
    ort::session::Session::builder()?.commit_from_file(model_path)
}

/// Lazily-loaded ONNX weights plus tokenizer.
struct LoadedOnnx {
    tokenizer: tokenizers::Tokenizer,
    pool: SessionPool,
}

/// Transformers.js-compatible ONNX embedding model (see module docs).
///
/// Construct with [`OnnxEmbeddingModel::from_plan`] using the resolved
/// `ModelBuildPlan::TransformersJs { entry, cache_dir }` fields plus the
/// requested [`DeviceKind`], then either call `prepare` (reports download
/// progress) or embed directly — the first [`EmbeddingModel::embed`]
/// loads the model.
pub struct OnnxEmbeddingModel {
    info: EmbeddingModelInfo,
    entry: TransformersJsEntry,
    cache_dir: PathBuf,
    device: DeviceKind,
    loaded: OnceLock<Result<LoadedOnnx, EngineError>>,
}

impl OnnxEmbeddingModel {
    /// Builds the backend from the resolved factory plan fields for the
    /// `ModelBuildPlan::TransformersJs` arm (catalog entry plus cache
    /// directory) and the requested device for CPU-fallback warnings.
    #[must_use]
    pub fn from_plan(entry: TransformersJsEntry, cache_dir: PathBuf, device: DeviceKind) -> Self {
        let info = EmbeddingModelInfo {
            reference: entry.reference.to_owned(),
            provider: entry.provider.to_owned(),
            model: entry.model.to_owned(),
            dimension: entry.dimension,
            metric: SearchMetric::Cosine,
            supports_images: false,
            max_input_tokens: Some(entry.max_input_tokens),
            input_kinds: vec![EmbeddingInputKind::Text],
            endpoint: None,
            default_concurrency: None,
        };
        Self {
            info,
            entry,
            cache_dir,
            device,
            loaded: OnceLock::new(),
        }
    }

    /// Downloads (when the cache misses) and loads tokenizer plus model,
    /// reporting through `sink`. Idempotent: later calls reuse the load.
    ///
    /// # Errors
    ///
    /// Returns an error if the download fails or the tokenizer/model cannot
    /// be loaded from the cache.
    pub fn prepare(&self, sink: Option<ModelLoadSink>) -> EngineResult<()> {
        self.ensure_loaded(sink)?;
        Ok(())
    }

    fn ensure_loaded(&self, sink: Option<ModelLoadSink>) -> EngineResult<&LoadedOnnx> {
        self.loaded
            .get_or_init(|| self.load(sink))
            .as_ref()
            .map_err(Clone::clone)
    }

    fn load(&self, sink: Option<ModelLoadSink>) -> EngineResult<LoadedOnnx> {
        use crate::models::catalog::ModelReference;
        let reference = ModelReference::from(self.entry.reference);
        let remote_onnx = onnx_remote_file(&self.entry);
        let mut reporter =
            ModelDownloadReporter::new(&reference, sink, &[remote_onnx, "tokenizer.json"]);
        reporter.start();
        if !matches!(self.device, DeviceKind::Auto | DeviceKind::Cpu) {
            let _ = reporter.warning(&format!(
                "ONNX {} execution is unavailable, falling back to CPU.",
                self.device.as_str()
            ));
        }
        let loaded = self.load_inner(&mut reporter, remote_onnx);
        match loaded {
            Ok(loaded) => {
                reporter.finish();
                Ok(loaded)
            }
            Err(err) => {
                let _ = reporter.warning(
                    "Unable to prepare the local embedding model. Check network access and the model cache.",
                );
                Err(err)
            }
        }
    }

    /// Local paths of the ONNX weights and `tokenizer.json`, shared by the
    /// loader and the [`EmbeddingModel::is_cached`] probe so the two can
    /// never disagree about what "cached" means.
    fn artifact_paths(&self, remote_onnx: &str) -> (PathBuf, String, PathBuf) {
        // `rsplit('/').next()` yields `Some` even without a `/`, but the
        // catalog paths always carry the `onnx/` prefix — fall back to the
        // full remote path rather than panicking if that ever changes.
        let onnx_file_name = match remote_onnx.rsplit('/').next() {
            Some(name) if !name.is_empty() => name,
            _ => remote_onnx,
        };
        let model_path = download::scoped_cache_path(
            &self.cache_dir,
            "onnx",
            self.entry.repo,
            self.entry.revision,
            onnx_file_name,
        );
        let tokenizer_path = download::scoped_cache_path(
            &self.cache_dir,
            "onnx",
            self.entry.repo,
            self.entry.revision,
            "tokenizer.json",
        );
        (model_path, onnx_file_name.to_owned(), tokenizer_path)
    }

    fn load_inner(
        &self,
        reporter: &mut ModelDownloadReporter,
        remote_onnx: &str,
    ) -> EngineResult<LoadedOnnx> {
        let entry = &self.entry;
        let (model_path, onnx_file_name, tokenizer_path) = self.artifact_paths(remote_onnx);
        download::download_cached_file(
            entry.repo,
            entry.revision,
            remote_onnx,
            &model_path,
            &onnx_file_name,
            reporter,
        )
        .map_err(|err| embed_failed(entry, format_args!("{err}")))?;
        // Quantized models externalize weights next to the graph
        // (`model_q4.onnx` + `model_q4.onnx_data`): without the companion,
        // the session fails to load. Its presence is probed with a HEAD
        // request first — a 404 is the normal "no external data" answer,
        // while a present-but-unfetchable file stays a loud error below.
        let data_remote = format!("{remote_onnx}_data");
        let data_file_name = format!("{onnx_file_name}_data");
        if remote_file_exists(&download::huggingface_url(
            entry.repo,
            entry.revision,
            &data_remote,
        )) {
            reporter.register(&data_file_name);
            let data_path = download::scoped_cache_path(
                &self.cache_dir,
                "onnx",
                entry.repo,
                entry.revision,
                &data_file_name,
            );
            download::download_cached_file(
                entry.repo,
                entry.revision,
                &data_remote,
                &data_path,
                &data_file_name,
                reporter,
            )
            .map_err(|err| embed_failed(entry, format_args!("{err}")))?;
        }
        download::download_cached_file(
            entry.repo,
            entry.revision,
            "tokenizer.json",
            &tokenizer_path,
            "tokenizer.json",
            reporter,
        )
        .map_err(|err| embed_failed(entry, format_args!("{err}")))?;
        let mut tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path)
            .map_err(|err| embed_failed(entry, format_args!("load tokenizer: {err}")))?;
        // The hub `tokenizer.json` may bake in padding/truncation
        // (minilm pads every encoding to 128 with zeros, which the mask
        // would then mistake for real tokens). Batching, padding, and
        // truncation are owned by `embed_core`, so the encoder must return
        // raw ids exactly like the TS pipeline inputs.
        tokenizer.with_padding(None);
        // `with_truncation` is fallible: discarding the `Result` would
        // silently keep baked-in truncation (the minilm failure mode
        // above), so a failure aborts the load instead.
        tokenizer
            .with_truncation(None)
            .map_err(|err| embed_failed(entry, format_args!("clear truncation: {err}")))?;
        Ok(LoadedOnnx {
            tokenizer,
            pool: SessionPool::new(*entry, model_path),
        })
    }

    fn embed_core(
        &self,
        loaded: &LoadedOnnx,
        purpose: EmbeddingPurpose,
        inputs: &[EmbeddingInput<'_>],
    ) -> EngineResult<EmbeddingResult> {
        let entry = &self.entry;
        let mut texts = Vec::with_capacity(inputs.len());
        for input in inputs {
            match input {
                EmbeddingInput::Text { text } => {
                    let prefixed =
                        match purpose_prefix(entry.query_prefix, entry.document_prefix, purpose) {
                            Some(prefix) => format!("{prefix}{text}"),
                            None => (*text).to_owned(),
                        };
                    texts.push(prefixed);
                }
                EmbeddingInput::Image { .. } => {
                    return Err(EngineError::from(ModelError::UnsupportedImage {
                        reference: entry.reference.to_owned(),
                        index: None,
                    }));
                }
            }
        }
        // Tokenize without truncation first so over-long inputs are flagged
        // exactly like `findTruncatedInputIndexes` (encode at
        // `max_input_tokens + 1`, flag past `max_input_tokens`).
        let mut ids_batch = Vec::with_capacity(texts.len());
        let mut truncated = Vec::new();
        for (index, text) in texts.iter().enumerate() {
            let encoding = loaded
                .tokenizer
                .encode(text.as_str(), true)
                .map_err(|err| tokenize_failed(entry, err))?;
            let mut ids: Vec<i64> = encoding.get_ids().iter().map(|id| i64::from(*id)).collect();
            if ids.len() > entry.max_input_tokens {
                truncated.push(index);
                ids.truncate(entry.max_input_tokens);
            }
            ids_batch.push(ids);
        }
        let sequence_len = ids_batch.iter().map(Vec::len).max().unwrap_or(0).max(1);
        let batch_len = texts.len();
        let mut input_ids = Vec::with_capacity(batch_len.saturating_mul(sequence_len));
        let mut attention_mask = Vec::with_capacity(batch_len.saturating_mul(sequence_len));
        for ids in &ids_batch {
            for position in 0..sequence_len {
                match ids.get(position) {
                    Some(id) => {
                        input_ids.push(*id);
                        attention_mask.push(1_i64);
                    }
                    None => {
                        input_ids.push(0);
                        attention_mask.push(0);
                    }
                }
            }
        }
        // `Session::run` takes `&mut self`: the lease owns the only
        // reference, so each checkout is an exclusive run — exactly the
        // bound the pool exists to enforce.
        let mut lease = loaded.pool.acquire()?;
        let session = lease
            .session
            .as_mut()
            .ok_or_else(|| embed_failed(entry, "session pool lease was empty"))?;
        let wants_token_types = session
            .inputs()
            .iter()
            .any(|input| input.name() == "token_type_ids");
        let input_ids = ort::value::Tensor::from_array((vec![batch_len, sequence_len], input_ids))
            .map_err(|err| embed_failed(entry, format_args!("build input_ids: {err}")))?;
        let attention_mask =
            ort::value::Tensor::from_array((vec![batch_len, sequence_len], attention_mask))
                .map_err(|err| embed_failed(entry, format_args!("build attention_mask: {err}")))?;
        let mut outputs = if wants_token_types {
            let token_type_ids = ort::value::Tensor::from_array((
                vec![batch_len, sequence_len],
                vec![0_i64; batch_len.saturating_mul(sequence_len)],
            ))
            .map_err(|err| embed_failed(entry, format_args!("build token_type_ids: {err}")))?;
            session
                .run(ort::inputs![
                    "input_ids" => input_ids,
                    "attention_mask" => attention_mask,
                    "token_type_ids" => token_type_ids
                ])
                .map_err(|err| embed_failed(entry, format_args!("onnx inference: {err}")))?
        } else {
            session
                .run(ort::inputs![
                    "input_ids" => input_ids,
                    "attention_mask" => attention_mask
                ])
                .map_err(|err| embed_failed(entry, format_args!("onnx inference: {err}")))?
        };
        // Every feature-extraction model emits `last_hidden_state`; a
        // missing output means the cached artifact is not the model the
        // catalog names, so report it rather than guessing another output.
        let output = outputs.remove("last_hidden_state").ok_or_else(|| {
            EngineError::from(ModelError::TransformersJsInvalidTensor {
                reference: entry.reference.to_owned(),
                detail: "session returned no last_hidden_state output".to_owned(),
            })
        })?;
        // Borrowed `(&Shape, &[f32])`: no copy of the hidden states, and
        // the borrow ends inside this function.
        let (shape, data) = output.try_extract_tensor::<f32>().map_err(|err| {
            EngineError::from(ModelError::TransformersJsInvalidTensor {
                reference: entry.reference.to_owned(),
                detail: format!("extract last_hidden_state: {err}"),
            })
        })?;
        let dims: Vec<usize> = shape.iter().map(|dim| *dim as usize).collect();
        match dims.as_slice() {
            [batch, sequence, dimension]
                if *batch == batch_len
                    && *sequence == sequence_len
                    && *dimension == entry.dimension =>
            {
                pool_hidden_states(entry, data, &ids_batch, sequence_len, *dimension)
                    .map(|vectors| EmbeddingResult { vectors, truncated })
            }
            _ => Err(EngineError::from(ModelError::TransformersJsInvalidTensor {
                reference: entry.reference.to_owned(),
                detail: format!(
                    "expected={batch_len}x{seq}x{dim} actual={dims:?}",
                    seq = sequence_len,
                    dim = entry.dimension
                ),
            })),
        }
    }
}

impl EmbeddingModel for OnnxEmbeddingModel {
    fn info(&self) -> &EmbeddingModelInfo {
        &self.info
    }

    fn max_batch_size(&self) -> usize {
        self.entry.max_batch_size
    }

    fn is_cached(&self) -> bool {
        let remote_onnx = onnx_remote_file(&self.entry);
        let (model_path, _, tokenizer_path) = self.artifact_paths(remote_onnx);
        download::is_usable_file(&model_path) && download::is_usable_file(&tokenizer_path)
    }

    fn prepare(&self, sink: Option<ModelLoadSink>) -> EngineResult<()> {
        self.ensure_loaded(sink)?;
        Ok(())
    }

    fn embed(
        &self,
        purpose: EmbeddingPurpose,
        inputs: &[EmbeddingInput<'_>],
    ) -> EngineResult<EmbeddingResult> {
        embed_validated(self, inputs, || {
            let loaded = self.ensure_loaded(None)?;
            self.embed_core(loaded, purpose, inputs)
        })
    }
}

/// Reduces `[batch, sequence, dimension]` hidden states to one vector per
/// input: row 0 for `cls`, the mask-weighted mean for `mean`, mirroring
/// the pipeline's `pooling` option. Rejects non-finite components with
/// `INVALID_TENSOR` like `tensorToVectors`, and L2-normalizes when the
/// catalog entry asks for it.
fn pool_hidden_states(
    entry: &TransformersJsEntry,
    data: &[f32],
    ids_batch: &[Vec<i64>],
    sequence_len: usize,
    dimension: usize,
) -> EngineResult<Vec<Vec<f32>>> {
    let mut vectors = Vec::with_capacity(ids_batch.len());
    for (input_index, ids) in ids_batch.iter().enumerate() {
        let base = input_index
            .saturating_mul(sequence_len)
            .saturating_mul(dimension);
        let mut vector = vec![0f32; dimension];
        match entry.pooling {
            PoolingKind::Cls => {
                let row = data
                    .get(base..base.saturating_add(dimension))
                    .ok_or_else(|| {
                        EngineError::from(ModelError::TransformersJsInvalidTensor {
                            reference: entry.reference.to_owned(),
                            detail: format!("index={input_index} offset=0"),
                        })
                    })?;
                vector.copy_from_slice(row);
            }
            PoolingKind::Mean => {
                // Real (non-padded) length: ids were truncated before
                // padding, so `ids.len()` is the mask-weighted count.
                let real_len = ids.len().min(sequence_len).max(1);
                for position in 0..real_len {
                    let start = base.saturating_add(position.saturating_mul(dimension));
                    let row = data
                        .get(start..start.saturating_add(dimension))
                        .ok_or_else(|| {
                            EngineError::from(ModelError::TransformersJsInvalidTensor {
                                reference: entry.reference.to_owned(),
                                detail: format!("index={input_index} offset={position}"),
                            })
                        })?;
                    for (acc, value) in vector.iter_mut().zip(row.iter()) {
                        *acc += *value;
                    }
                }
                let count = real_len as f32;
                for value in vector.iter_mut() {
                    *value /= count;
                }
            }
        }
        for (offset, value) in vector.iter().enumerate() {
            if !value.is_finite() {
                return Err(EngineError::from(ModelError::TransformersJsInvalidTensor {
                    reference: entry.reference.to_owned(),
                    detail: format!("index={input_index} offset={offset}"),
                }));
            }
        }
        if entry.normalize {
            let squared_norm: f32 = vector.iter().map(|value| value * value).sum();
            if squared_norm > 0.0 {
                let inverse_norm = 1.0 / squared_norm.sqrt();
                for value in vector.iter_mut() {
                    *value *= inverse_norm;
                }
            }
        }
        vectors.push(vector);
    }
    Ok(vectors)
}

#[cfg(test)]
// Pooling assertions index fixed-shape test vectors; a panic here is just
// a test failure.
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::models::catalog::TransformersDtype;

    fn entry(pooling: PoolingKind) -> TransformersJsEntry {
        TransformersJsEntry {
            reference: "local/test-onnx",
            provider: "local",
            model: "test-onnx",
            repo: "org/test-onnx",
            revision: "rev",
            dtype: TransformersDtype::Q4,
            dimension: 3,
            pooling,
            normalize: true,
            query_prefix: Some("query: "),
            document_prefix: Some("passage: "),
            max_input_tokens: 512,
            max_batch_size: 4,
        }
    }

    #[test]
    fn dtype_selects_onnx_artifact() {
        let kinds = [
            (TransformersDtype::Q4, "onnx/model_q4.onnx"),
            (TransformersDtype::Q8, "onnx/model_quantized.onnx"),
            (TransformersDtype::Fp32, "onnx/model.onnx"),
        ];
        for (dtype, file) in kinds {
            let mut candidate = entry(PoolingKind::Cls);
            candidate.dtype = dtype;
            assert_eq!(onnx_remote_file(&candidate), file);
        }
    }

    #[test]
    fn cls_pooling_takes_first_row_and_normalizes() {
        let candidate = entry(PoolingKind::Cls);
        // Two inputs, sequence 2, dimension 3: rows are [3,4,0] / [0,0,0].
        let data = vec![
            3.0_f32, 4.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 5.0, 5.0, 5.0,
        ];
        let ids = vec![vec![1_i64, 2], vec![1]];
        let vectors = pool_hidden_states(&candidate, &data, &ids, 2, 3).unwrap();
        assert_eq!(vectors.len(), 2);
        // [3,4,0] normalized; [1,0,0] normalized.
        assert!((vectors[0][0] - 0.6).abs() < 1e-6);
        assert!((vectors[0][1] - 0.8).abs() < 1e-6);
        assert_eq!(vectors[1], vec![1.0, 0.0, 0.0]);
    }

    #[test]
    fn mean_pooling_averages_unpadded_rows() {
        let candidate = entry(PoolingKind::Mean);
        // Second input has one real token: padding row must not count.
        let data = vec![
            1.0_f32, 1.0, 1.0, 3.0, 3.0, 3.0, 2.0, 0.0, 0.0, 9.0, 9.0, 9.0,
        ];
        let ids = vec![vec![1_i64, 2], vec![1]];
        let vectors = pool_hidden_states(&candidate, &data, &ids, 2, 3).unwrap();
        assert_eq!(vectors.len(), 2);
        // Mean of [1,1,1] and [3,3,3] = [2,2,2], normalized.
        let expected = 1.0 / 3.0_f32.sqrt();
        for value in &vectors[0] {
            assert!((value - expected).abs() < 1e-6);
        }
        // Single real row [2,0,0] normalized.
        assert_eq!(vectors[1], vec![1.0, 0.0, 0.0]);
    }

    #[test]
    fn non_finite_pool_output_is_rejected() {
        let candidate = entry(PoolingKind::Cls);
        let data = vec![f32::NAN, 0.0, 0.0];
        let ids = vec![vec![1_i64]];
        let err = pool_hidden_states(&candidate, &data, &ids, 1, 3).unwrap_err();
        assert_eq!(
            err.code().to_string(),
            "ZVEC_GREP.ENGINE.MODELS.TRANSFORMERS_JS_INVALID_TENSOR"
        );
    }
}

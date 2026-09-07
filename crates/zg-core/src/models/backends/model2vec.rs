//! Static word-embedding (model2vec/potion) backend.
//!
//! Mirrors `src/engine/models/backends/model2vec.ts`,
//! `model2vec-runtime.ts`, and `model2vec-tokenizer.ts`: artifacts are
//! downloaded from Hugging Face into the shared model cache, the named
//! safetensors tensor becomes a static embedding table, and each text is
//! embedded by tokenizing (truncating to `max_input_tokens + 1` like the
//! TypeScript tokenizer, flagging inputs longer than `max_input_tokens`),
//! dropping unknown-token ids, mean-pooling the token rows, and L2
//! normalizing when the catalog entry asks for it.
//!
//! The TypeScript worker-thread pool becomes scoped `std::thread` workers
//! splitting one batch into `concurrency` chunks. The `tokenizers` crate
//! loads `tokenizer.json` directly and the `safetensors` crate reads the
//! weight file; F16 weights are converted to `f32` once at load so the hot
//! path is plain `f32` arithmetic (numerically identical to the TypeScript
//! half-float lookup table, since every half converts to `f32` exactly).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use safetensors::Dtype;

use crate::error::{EngineError, EngineResult};

use crate::models::catalog::{Model2VecEntry, ModelReference};
use crate::models::download::{self, ModelDownloadReporter};
use crate::models::embeddings::{EmbeddingResult, embed_validated};
use crate::models::error::ModelError;
use crate::models::{
    EmbeddingInput, EmbeddingInputKind, EmbeddingModel, EmbeddingModelInfo, EmbeddingPurpose,
    ModelLoadSink,
};
use crate::types::SearchMetric;

/// Tokenizer configuration stub written next to a cached `tokenizer.json`
/// when the hub file is absent, mirroring `resolveTokenizerSource`.
const TOKENIZER_CONFIG_STUB: &str = "{\"tokenizer_class\":\"PreTrainedTokenizer\"}\n";

/// Static embedding table: one `dimension`-wide row per vocabulary id.
struct EmbeddingTable {
    data: Vec<f32>,
    rows: usize,
    dimension: usize,
}

/// Lazily-loaded model2vec weights plus tokenizer.
struct LoadedModel2Vec {
    tokenizer: tokenizers::Tokenizer,
    unk_id: Option<u32>,
    table: EmbeddingTable,
}

/// Static word-embedding model: local safetensors weights plus tokenizer.
///
/// Construct with [`Model2VecEmbeddingModel::from_plan`] using the resolved
/// `ModelBuildPlan::Model2Vec { entry, cache_dir }` fields, then either call
/// [`Model2VecEmbeddingModel::prepare`] (reports download progress) or embed
/// directly — the first [`EmbeddingModel::embed`] loads the model.
pub struct Model2VecEmbeddingModel {
    info: EmbeddingModelInfo,
    entry: Model2VecEntry,
    cache_dir: PathBuf,
    concurrency: usize,
    loaded: OnceLock<Result<LoadedModel2Vec, EngineError>>,
}

/// Builds the typed `MODELS.MODEL2VEC_LOAD_FAILED` error for `entry`.
fn load_failed(entry: &Model2VecEntry, detail: impl std::fmt::Display) -> EngineError {
    EngineError::from(ModelError::Model2VecLoad {
        reference: entry.reference.to_owned(),
        repo: entry.repo.to_owned(),
        revision: entry.revision.to_owned(),
        detail: detail.to_string(),
    })
}

/// Builds the typed `MODELS.MODEL2VEC_DOWNLOAD_FAILED` error for `entry`.
fn download_failed(entry: &Model2VecEntry, detail: impl std::fmt::Display) -> EngineError {
    EngineError::from(ModelError::Model2VecDownload {
        reference: entry.reference.to_owned(),
        repo: entry.repo.to_owned(),
        revision: entry.revision.to_owned(),
        detail: detail.to_string(),
    })
}

impl Model2VecEmbeddingModel {
    /// Builds the backend from the resolved factory plan fields for the
    /// `ModelBuildPlan::Model2Vec` arm (catalog entry plus cache directory).
    pub fn from_plan(entry: Model2VecEntry, cache_dir: PathBuf) -> Self {
        let info = EmbeddingModelInfo {
            reference: entry.reference.to_owned(),
            provider: entry.provider.to_owned(),
            model: entry.model.to_owned(),
            dimension: entry.dimension,
            metric: SearchMetric::Cosine,
            supports_images: false,
            max_input_tokens: Some(entry.max_input_tokens),
            input_kinds: vec![EmbeddingInputKind::Text],
            default_concurrency: Some(entry.default_concurrency),
        };
        Self {
            info,
            entry,
            cache_dir,
            concurrency: entry.default_concurrency.max(1),
            loaded: OnceLock::new(),
        }
    }

    /// Downloads (when the cache misses) and loads weights plus tokenizer,
    /// reporting through `sink`. Idempotent: later calls reuse the load.
    pub fn prepare(&self, sink: Option<ModelLoadSink>) -> EngineResult<()> {
        self.ensure_loaded(sink)?;
        Ok(())
    }

    fn ensure_loaded(&self, sink: Option<ModelLoadSink>) -> EngineResult<&LoadedModel2Vec> {
        self.loaded
            .get_or_init(|| self.load(sink))
            .as_ref()
            .map_err(Clone::clone)
    }

    fn load(&self, sink: Option<ModelLoadSink>) -> EngineResult<LoadedModel2Vec> {
        let reference = ModelReference::from(self.entry.reference);
        let mut reporter = ModelDownloadReporter::new(
            &reference,
            sink,
            &[self.entry.model_file, self.entry.tokenizer_file],
        );
        reporter.start();
        let loaded = self.load_inner(&mut reporter);
        match loaded {
            Ok(loaded) => {
                reporter.finish();
                Ok(loaded)
            }
            Err(err) => {
                reporter.warning(
                    "Unable to prepare the local embedding model. Check network access and the model cache.",
                );
                Err(err)
            }
        }
    }

    fn load_inner(&self, reporter: &mut ModelDownloadReporter) -> EngineResult<LoadedModel2Vec> {
        let entry = &self.entry;
        let model_file_name = Path::new(entry.model_file)
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| load_failed(entry, "invalid model file name"))?;
        let model_path = download::scoped_cache_path(
            &self.cache_dir,
            "model2vec",
            entry.repo,
            entry.revision,
            model_file_name,
        );
        let tokenizer_json_path = download::scoped_cache_path(
            &self.cache_dir,
            "model2vec",
            entry.repo,
            entry.revision,
            "tokenizer/tokenizer.json",
        );
        // `download_cached_file` skips usable files and reports progress for
        // downloads, exactly like `resolveCachedFile`.
        download::download_cached_file(
            entry.repo,
            entry.revision,
            entry.model_file,
            &model_path,
            model_file_name,
            reporter,
        )
        .map_err(|err| download_failed(entry, format_args!("{err}")))?;
        download::download_cached_file(
            entry.repo,
            entry.revision,
            entry.tokenizer_file,
            &tokenizer_json_path,
            entry.tokenizer_file,
            reporter,
        )
        .map_err(|err| download_failed(entry, format_args!("{err}")))?;
        if let Some(dir) = tokenizer_json_path.parent() {
            let config_path = dir.join("tokenizer_config.json");
            if !download::is_usable_file(&config_path) {
                fs::write(&config_path, TOKENIZER_CONFIG_STUB).map_err(|err| {
                    load_failed(entry, format_args!("write tokenizer config: {err}"))
                })?;
            }
        }
        let table = read_static_embedding_table(
            &model_path,
            entry.embedding_tensor,
            entry.dimension,
            entry.reference,
            entry.repo,
            entry.revision,
        )?;
        let tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_json_path)
            .map_err(|err| load_failed(entry, format_args!("load tokenizer: {err}")))?;
        let unk_id = resolve_unknown_token_id(&tokenizer_json_path, &tokenizer);
        Ok(LoadedModel2Vec {
            tokenizer,
            unk_id,
            table,
        })
    }

    fn embed_core(
        &self,
        loaded: &LoadedModel2Vec,
        inputs: &[EmbeddingInput<'_>],
    ) -> EngineResult<EmbeddingResult> {
        let mut texts = Vec::with_capacity(inputs.len());
        for input in inputs {
            match input {
                EmbeddingInput::Text { text } => texts.push(*text),
                EmbeddingInput::Image { .. } => {
                    return Err(EngineError::from(ModelError::UnsupportedImage {
                        reference: self.entry.reference.to_owned(),
                        index: None,
                    }));
                }
            }
        }
        let (vectors, truncated) = embed_texts_parallel(
            &loaded.tokenizer,
            loaded.unk_id,
            &loaded.table,
            &texts,
            self.entry.max_input_tokens,
            self.entry.normalize,
            self.concurrency,
            self.entry.reference,
        )
        .map_err(|err| {
            EngineError::from(ModelError::Model2VecEmbed {
                reference: self.entry.reference.to_owned(),
                repo: self.entry.repo.to_owned(),
                detail: err.to_string(),
            })
        })?;
        Ok(EmbeddingResult { vectors, truncated })
    }
}

impl EmbeddingModel for Model2VecEmbeddingModel {
    fn info(&self) -> &EmbeddingModelInfo {
        &self.info
    }

    fn max_batch_size(&self) -> usize {
        self.entry.max_batch_size
    }

    fn prepare(&self, sink: Option<ModelLoadSink>) -> EngineResult<()> {
        self.ensure_loaded(sink)?;
        Ok(())
    }

    fn embed(
        &self,
        _purpose: EmbeddingPurpose,
        inputs: &[EmbeddingInput<'_>],
    ) -> EngineResult<EmbeddingResult> {
        // Model2vec catalog entries define no purpose prefixes, so purpose
        // only flows through validation like the TypeScript `formatText`.
        embed_validated(self, inputs, || {
            let loaded = self.ensure_loaded(None)?;
            self.embed_core(loaded, inputs)
        })
    }
}

/// Reads the named 2-D tensor (`F16` or `F32`, `[rows, dimension]`) from a
/// safetensors file into an `f32` table, mirroring
/// `readStaticEmbeddingTable` (which instead keeps the raw bytes plus a
/// half-float lookup table).
fn read_static_embedding_table(
    path: &Path,
    tensor_name: &str,
    expected_dimension: usize,
    reference: &str,
    repo: &str,
    revision: &str,
) -> EngineResult<EmbeddingTable> {
    let load_failed = |detail: String| {
        EngineError::from(ModelError::Model2VecLoad {
            reference: reference.to_owned(),
            repo: repo.to_owned(),
            revision: revision.to_owned(),
            detail,
        })
    };
    let bytes = fs::read(path).map_err(|err| load_failed(format!("read weights: {err}")))?;
    let parsed = safetensors::SafeTensors::deserialize(&bytes)
        .map_err(|err| load_failed(format!("safetensors header is invalid: {err}")))?;
    let view = parsed
        .tensor(tensor_name)
        .map_err(|err| load_failed(format!("tensor '{tensor_name}' is missing: {err}")))?;
    match view.dtype() {
        Dtype::F16 | Dtype::F32 => {}
        dtype => {
            return Err(load_failed(format!(
                "tensor '{tensor_name}' has incompatible dtype {dtype:?}"
            )));
        }
    }
    let shape = view.shape();
    if shape.len() != 2 || shape[1] != expected_dimension {
        return Err(load_failed(format!(
            "tensor '{tensor_name}' is missing or incompatible"
        )));
    }
    let rows = shape[0];
    let bytes_per_value = if view.dtype() == Dtype::F16 { 2 } else { 4 };
    let value_count = rows
        .checked_mul(expected_dimension)
        .ok_or_else(|| load_failed(format!("tensor '{tensor_name}' has invalid shape")))?;
    let expected_bytes = value_count
        .checked_mul(bytes_per_value)
        .ok_or_else(|| load_failed(format!("tensor '{tensor_name}' has invalid shape")))?;
    let raw = view.data();
    if raw.len() != expected_bytes {
        return Err(load_failed(format!(
            "tensor '{tensor_name}' has invalid offsets"
        )));
    }
    let mut data = Vec::with_capacity(value_count);
    if view.dtype() == Dtype::F16 {
        for chunk in raw.chunks_exact(2) {
            let bits = u16::from_le_bytes([chunk[0], chunk[1]]);
            data.push(half::f16::from_bits(bits).to_f32());
        }
    } else {
        for chunk in raw.chunks_exact(4) {
            data.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
    }
    Ok(EmbeddingTable {
        data,
        rows,
        dimension: expected_dimension,
    })
}

/// Resolves the unknown-token id from `tokenizer.json` (`model.unk_id`, else
/// `model.unk_token` mapped through the tokenizer), mirroring
/// `resolveUnknownTokenId`. Returns `None` when the tokenizer defines none.
fn resolve_unknown_token_id(
    tokenizer_json_path: &Path,
    tokenizer: &tokenizers::Tokenizer,
) -> Option<u32> {
    let text = fs::read_to_string(tokenizer_json_path).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&text).ok()?;
    let model = parsed.get("model")?;
    if let Some(id) = model.get("unk_id").and_then(serde_json::Value::as_u64) {
        if id <= u32::MAX as u64 {
            return Some(id as u32);
        }
    }
    let token = model.get("unk_token")?.as_str()?;
    tokenizer.token_to_id(token)
}

/// Tokenizes one text: encodes without special tokens, flags inputs longer
/// than `max_input_tokens` (the TypeScript tokenizer truncates at
/// `max_input_tokens + 1`, which is observably identical), keeps the first
/// `max_input_tokens` ids, and drops unknown-token ids.
fn tokenize_text(
    tokenizer: &tokenizers::Tokenizer,
    unk_id: Option<u32>,
    text: &str,
    max_input_tokens: usize,
    reference: &str,
) -> EngineResult<(Vec<u32>, bool)> {
    let encoding = tokenizer.encode(text, false).map_err(|err| {
        EngineError::from(ModelError::Model2VecTokenize {
            reference: reference.to_owned(),
            detail: err.to_string(),
        })
    })?;
    let mut ids: Vec<u32> = encoding.get_ids().to_vec();
    let was_truncated = ids.len() > max_input_tokens;
    ids.truncate(max_input_tokens);
    if let Some(unk) = unk_id {
        ids.retain(|id| *id != unk);
    }
    Ok((ids, was_truncated))
}

/// Mean-pools the static rows for `ids` and L2-normalizes when requested,
/// mirroring `embedStaticTokenList` (empty inputs yield the zero vector).
fn pool_token_ids(
    table: &EmbeddingTable,
    ids: &[u32],
    normalize: bool,
    reference: &str,
) -> EngineResult<Vec<f32>> {
    let mut vector = vec![0f32; table.dimension];
    if ids.is_empty() {
        return Ok(vector);
    }
    for id in ids {
        let row_index = *id as usize;
        if row_index >= table.rows {
            return Err(EngineError::from(ModelError::TokenOutOfRange {
                reference: reference.to_owned(),
                id: *id,
                rows: table.rows,
            }));
        }
        let start = row_index.saturating_mul(table.dimension);
        let end = start.saturating_add(table.dimension);
        let row = table.data.get(start..end).ok_or_else(|| {
            EngineError::from(ModelError::TokenOutOfRange {
                reference: reference.to_owned(),
                id: *id,
                rows: table.rows,
            })
        })?;
        for (acc, value) in vector.iter_mut().zip(row.iter()) {
            *acc += *value;
        }
    }
    let count = ids.len() as f32;
    let mut squared_norm = 0f32;
    for value in vector.iter_mut() {
        *value /= count;
        squared_norm += *value * *value;
    }
    if normalize && squared_norm > 0.0 {
        let inverse_norm = 1.0 / squared_norm.sqrt();
        for value in vector.iter_mut() {
            *value *= inverse_norm;
        }
    }
    Ok(vector)
}

/// One embedded chunk item: (input index, vector, was truncated). Counts are
/// small, but the alias keeps the scoped-thread join types readable.
type ChunkItem = (usize, Vec<f32>, bool);

/// Embeds a batch across `concurrency` scoped threads (the TypeScript worker
/// pool), preserving input order and returning sorted truncation indices.
#[allow(clippy::too_many_arguments)]
fn embed_texts_parallel(
    tokenizer: &tokenizers::Tokenizer,
    unk_id: Option<u32>,
    table: &EmbeddingTable,
    texts: &[&str],
    max_input_tokens: usize,
    normalize: bool,
    concurrency: usize,
    reference: &str,
) -> EngineResult<(Vec<Vec<f32>>, Vec<usize>)> {
    let input_count = texts.len();
    let worker_count = concurrency.max(1).min(input_count.max(1));
    let chunk_len = input_count.div_ceil(worker_count).max(1);
    let mut truncated: Vec<usize> = Vec::new();
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for (chunk_index, chunk) in texts.chunks(chunk_len).enumerate() {
            let base = chunk_index.saturating_mul(chunk_len);
            handles.push(scope.spawn(move || {
                let mut items = Vec::with_capacity(chunk.len());
                for (offset, text) in chunk.iter().enumerate() {
                    let (ids, was_truncated) =
                        tokenize_text(tokenizer, unk_id, text, max_input_tokens, reference)?;
                    let vector = pool_token_ids(table, &ids, normalize, reference)?;
                    items.push((base.saturating_add(offset), vector, was_truncated));
                }
                Ok::<Vec<ChunkItem>, EngineError>(items)
            }));
        }
        let mut joined: EngineResult<Vec<Vec<ChunkItem>>> = Ok(Vec::new());
        for handle in handles {
            match handle.join() {
                Ok(Ok(items)) => {
                    if let Ok(all) = joined.as_mut() {
                        all.push(items);
                    }
                }
                Ok(Err(err)) => {
                    joined = Err(err);
                    break;
                }
                Err(_) => {
                    joined = Err(EngineError::from(ModelError::WorkerFailed {
                        reference: reference.to_owned(),
                    }));
                    break;
                }
            }
        }
        joined.map(|chunks| {
            let mut flat = Vec::with_capacity(input_count);
            for items in chunks {
                flat.extend(items);
            }
            flat
        })
    })
    .map(|mut flat| {
        flat.sort_by_key(|(index, _, _)| *index);
        let mut vectors = Vec::with_capacity(input_count);
        for (index, vector, was_truncated) in flat {
            vectors.push(vector);
            if was_truncated {
                truncated.push(index);
            }
        }
        truncated.sort_unstable();
        (vectors, truncated)
    })
}

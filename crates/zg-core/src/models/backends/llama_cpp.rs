//! llama-cpp (GGUF) embedding backend, behind the `llama` feature.
//!
//! Mirrors `src/engine/models/backends/llama-cpp.ts`: the catalog's `hf:`
//! URI resolves to a GGUF file downloaded through the shared
//! [`crate::models::download`] cache and sniffed for the `GGUF` magic
//! (HTML error pages get their own code), prompts are formatted per
//! model format (`qwen3` instruction prefix vs `embeddinggemma`
//! task/title prefixes), inputs are truncated to the context size, and
//! one pooled sequence embedding is produced per text, decoded exactly
//! like upstream's `embeddings` example (`clear_kv_cache` → `decode` →
//! `embeddings_seq_ith`, no explicit pooling selection, raw unnormalized
//! vectors).
//!
//! Concurrency follows the C/D design note in `docs/ts-divergence.md`:
//! `llama-cpp-2`'s `LlamaBackend` is neither `Send` nor `Sync`, so no
//! inference state may cross threads at all — not even behind a mutex.
//! Each loaded model therefore owns one dedicated worker thread holding
//! the backend, the model, and a single reused context as plain locals;
//! [`EmbeddingModel::embed`] ships formatted texts over a channel and
//! blocks on the reply. Embeds serialize per model instance (matching
//! TS's CPU `parallelism: 1` default); batching still amortizes the cost
//! because one call embeds up to `max_batch_size` inputs.
//!
//! The `llama-cpp-2` dependency builds CPU-only (`default-features =
//! false`), so a non-CPU [`DeviceKind`] warns once through the load sink
//! and falls back to CPU, mirroring the TypeScript GPU-fallback path.

use std::fs;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc;

use crate::error::{EngineError, EngineResult};
use crate::models::catalog::{LlamaCppEntry, LlamaModelFormat, ModelReference};
use crate::models::download::{self, ModelDownloadReporter};
use crate::models::embeddings::{DeviceKind, EmbeddingResult, embed_validated};
use crate::models::error::ModelError;
use crate::models::{
    EmbeddingInput, EmbeddingInputKind, EmbeddingModel, EmbeddingModelInfo, EmbeddingPurpose,
    ModelLoadSink,
};
use crate::types::SearchMetric;

/// GGUF magic sniffed at the head of every download, mirroring `GGUF_MAGIC`.
const GGUF_MAGIC: &[u8; 4] = b"GGUF";
/// Bytes sniffed for magic/HTML detection, mirroring the TS 512-byte read.
const GGUF_SNIFF_LEN: usize = 512;

/// Builds the typed `LLAMA_CPP_EMBED_FAILED` error for `entry`.
fn embed_failed(entry: &LlamaCppEntry, detail: impl std::fmt::Display) -> EngineError {
    EngineError::from(ModelError::LlamaCppEmbed {
        reference: entry.reference.to_owned(),
        detail: detail.to_string(),
    })
}

/// llama.cpp GGUF embedding model.
///
/// Construct with [`LlamaCppEmbeddingModel::from_plan`] using the resolved
/// `ModelBuildPlan::LlamaCpp { entry, cache_dir }` fields plus the
/// requested [`DeviceKind`], then either call `prepare` (reports download
/// progress and starts the worker) or embed directly — the first
/// [`EmbeddingModel::embed`] loads the model.
pub struct LlamaCppEmbeddingModel {
    info: EmbeddingModelInfo,
    entry: LlamaCppEntry,
    cache_dir: PathBuf,
    device: DeviceKind,
    loaded: OnceLock<Result<LoadedLlama, EngineError>>,
}

/// Handle to the dedicated inference worker.
///
/// The worker thread owns the `LlamaBackend`, model, and context as plain
/// locals (none may cross threads); this handle carries only the job
/// channel, which is `Send + Sync`, so the backend stays shareable. When
/// the last handle drops, the channel closes and the worker exits — no
/// join needed, no thread leaked.
struct LoadedLlama {
    worker: mpsc::Sender<EmbedJob>,
}

/// One embedding batch for the worker: formatted texts in, vectors out.
struct EmbedJob {
    texts: Vec<String>,
    reply: mpsc::Sender<EngineResult<EmbeddingResult>>,
}

impl LlamaCppEmbeddingModel {
    /// Builds the backend from the resolved factory plan fields for the
    /// `ModelBuildPlan::LlamaCpp` arm (catalog entry plus cache directory)
    /// and the requested device for CPU-fallback warnings.
    #[must_use]
    pub fn from_plan(entry: LlamaCppEntry, cache_dir: PathBuf, device: DeviceKind) -> Self {
        let info = EmbeddingModelInfo {
            reference: entry.reference.to_owned(),
            provider: entry.provider.to_owned(),
            model: entry.model.to_owned(),
            dimension: entry.dimension,
            metric: SearchMetric::Cosine,
            supports_images: false,
            max_input_tokens: Some(entry.context_size),
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

    /// Downloads (when the cache misses), validates the GGUF file, and
    /// starts the inference worker, reporting through `sink`. Idempotent:
    /// later calls reuse the worker.
    ///
    /// # Errors
    ///
    /// Returns an error if the download fails, the GGUF file is invalid, or
    /// the worker cannot start.
    pub fn prepare(&self, sink: Option<ModelLoadSink>) -> EngineResult<()> {
        self.ensure_loaded(sink)?;
        Ok(())
    }

    fn ensure_loaded(&self, sink: Option<ModelLoadSink>) -> EngineResult<&LoadedLlama> {
        self.loaded
            .get_or_init(|| self.load(sink))
            .as_ref()
            .map_err(Clone::clone)
    }

    fn load(&self, sink: Option<ModelLoadSink>) -> EngineResult<LoadedLlama> {
        let reference = ModelReference::from(self.entry.reference);
        let file_name = gguf_file_name(&self.entry);
        let mut reporter = ModelDownloadReporter::new(&reference, sink, &[file_name]);
        reporter.start();
        if !matches!(self.device, DeviceKind::Auto | DeviceKind::Cpu) {
            let _ = reporter.warning(&format!(
                "llama.cpp {} execution is unavailable, falling back to CPU.",
                self.device.as_str()
            ));
        }
        let loaded = self.load_inner(&mut reporter, file_name);
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

    fn load_inner(
        &self,
        reporter: &mut ModelDownloadReporter,
        file_name: &str,
    ) -> EngineResult<LoadedLlama> {
        let entry = &self.entry;
        let (repo, remote_file) = split_hf_uri(entry.uri)
            .ok_or_else(|| embed_failed(entry, format_args!("invalid model uri {}", entry.uri)))?;
        // The catalog pins no revision for GGUF entries (like TS
        // `catalog.ts:6-31`), so `main` is the resolution, not a freeze.
        let model_path =
            download::scoped_cache_path(&self.cache_dir, "llama-cpp", repo, "main", file_name);
        download::download_cached_file(repo, "main", remote_file, &model_path, file_name, reporter)
            .map_err(|err| embed_failed(entry, format_args!("{err}")))?;
        validate_gguf_file(entry, &model_path)?;
        spawn_worker(entry, model_path)
    }

    fn embed_core(
        &self,
        loaded: &LoadedLlama,
        purpose: EmbeddingPurpose,
        inputs: &[EmbeddingInput<'_>],
    ) -> EngineResult<EmbeddingResult> {
        let entry = &self.entry;
        let mut texts = Vec::with_capacity(inputs.len());
        for input in inputs {
            match input {
                EmbeddingInput::Text { text } => {
                    texts.push(format_text_for_embedding(text, purpose, entry.format));
                }
                EmbeddingInput::Image { .. } => {
                    return Err(EngineError::from(ModelError::UnsupportedImage {
                        reference: entry.reference.to_owned(),
                        index: None,
                    }));
                }
            }
        }
        let (reply_tx, reply_rx) = mpsc::channel();
        loaded
            .worker
            .send(EmbedJob {
                texts,
                reply: reply_tx,
            })
            .map_err(|_| embed_failed(entry, "embedding worker is unavailable"))?;
        reply_rx
            .recv()
            .map_err(|_| embed_failed(entry, "embedding worker stopped"))?
    }
}

impl EmbeddingModel for LlamaCppEmbeddingModel {
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
        purpose: EmbeddingPurpose,
        inputs: &[EmbeddingInput<'_>],
    ) -> EngineResult<EmbeddingResult> {
        embed_validated(self, inputs, || {
            let loaded = self.ensure_loaded(None)?;
            self.embed_core(loaded, purpose, inputs)
        })
    }
}

/// Starts the dedicated inference worker and blocks until it is ready:
/// backend init plus model load happen on the worker thread, so their
/// failures surface here through the handshake instead of the first
/// `embed`.
fn spawn_worker(entry: &LlamaCppEntry, model_path: PathBuf) -> EngineResult<LoadedLlama> {
    let (job_tx, job_rx) = mpsc::channel::<EmbedJob>();
    let (ready_tx, ready_rx) = mpsc::channel::<EngineResult<()>>();
    let worker_entry = *entry;
    std::thread::Builder::new()
        .name(format!("zg-llama-{}", entry.model))
        .spawn(move || worker_main(worker_entry, model_path, job_rx, ready_tx))
        .map_err(|err| embed_failed(entry, format_args!("start embedding worker: {err}")))?;
    ready_rx
        .recv()
        .map_err(|_| embed_failed(entry, "embedding worker stopped during startup"))?
        .map(|()| LoadedLlama { worker: job_tx })
}

/// Worker entry point: backend, model, and context live here as plain
/// locals for the thread's whole life (nested lifetimes, no
/// self-reference), serving one batch at a time until every handle
/// drops and the channel closes.
fn worker_main(
    entry: LlamaCppEntry,
    model_path: PathBuf,
    jobs: mpsc::Receiver<EmbedJob>,
    ready: mpsc::Sender<EngineResult<()>>,
) {
    use llama_cpp_2::context::params::LlamaContextParams;
    use llama_cpp_2::llama_backend::LlamaBackend;
    let backend = match LlamaBackend::init() {
        Ok(backend) => backend,
        Err(err) => {
            let _ = ready.send(Err(embed_failed(
                &entry,
                format_args!("init llama backend: {err}"),
            )));
            return;
        }
    };
    let model = match load_model_on_worker(&backend, &model_path, &entry) {
        Ok(model) => model,
        Err(err) => {
            let _ = ready.send(Err(err));
            return;
        }
    };
    // Unspecified pooling resolves inside llama.cpp exactly like the TS
    // backend, which passes no pooling option either; thread counts stay
    // at their automatic defaults like the TS CPU path.
    let params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(entry.context_size as u32))
        .with_embeddings(true);
    let mut context = match model.new_context(&backend, params) {
        Ok(context) => context,
        Err(err) => {
            let _ = ready.send(Err(embed_failed(
                &entry,
                format_args!("create embedding context: {err}"),
            )));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    for job in jobs {
        let result = embed_texts(&entry, &model, &mut context, &job.texts);
        if job.reply.send(result).is_err() {
            break;
        }
    }
}

/// Loads the model on the worker thread; split out so the handshake
/// reports load failures before the serve loop starts.
fn load_model_on_worker(
    backend: &llama_cpp_2::llama_backend::LlamaBackend,
    model_path: &Path,
    entry: &LlamaCppEntry,
) -> EngineResult<llama_cpp_2::model::LlamaModel> {
    use llama_cpp_2::model::params::LlamaModelParams;
    llama_cpp_2::model::LlamaModel::load_from_file(
        backend,
        model_path,
        &LlamaModelParams::default(),
    )
    .map_err(|err| embed_failed(entry, format_args!("load gguf model: {err}")))
}

/// Embeds one batch on the worker's reused context, mirroring upstream's
/// `embeddings` example: tokenize, truncate, one sequence per batch,
/// `clear_kv_cache` → `decode` → pooled sequence embedding. Vectors are
/// raw like the TS backend (no L2 normalization); output validation runs
/// in [`embed_validated`](crate::models::embeddings::embed_validated).
fn embed_texts(
    entry: &LlamaCppEntry,
    model: &llama_cpp_2::model::LlamaModel,
    context: &mut llama_cpp_2::context::LlamaContext<'_>,
    texts: &[String],
) -> EngineResult<EmbeddingResult> {
    use llama_cpp_2::llama_batch::LlamaBatch;
    use llama_cpp_2::model::AddBos;
    // TS `truncateToContextSize` reads the model's own training limit;
    // fall back to the catalog context size when the model reports none.
    let train_ctx = model.n_ctx_train().max(1);
    let limit = (entry.context_size as u32).min(train_ctx).max(1) as usize;
    let mut vectors = Vec::with_capacity(texts.len());
    let mut truncated = Vec::new();
    for (index, text) in texts.iter().enumerate() {
        let tokens = model
            .str_to_token(text, AddBos::Always)
            .map_err(|err| embed_failed(entry, format_args!("tokenize: {err}")))?;
        // Over-long inputs keep the first `limit - 4` tokens and are
        // flagged. Token ids feed the batch directly instead of
        // detokenizing first — observably identical ids without the text
        // round-trip.
        let kept = if tokens.len() > limit {
            truncated.push(index);
            // `end` is clamped to `tokens.len()`, so the `get` below only
            // fails if the length changed concurrently (it cannot: local).
            let end = limit.saturating_sub(4).max(1).min(tokens.len());
            tokens.get(..end).map_or_else(Vec::new, <[_]>::to_vec)
        } else {
            tokens
        };
        let mut batch = LlamaBatch::new(kept.len().max(1), 1);
        batch
            .add_sequence(&kept, 0, false)
            .map_err(|err| embed_failed(entry, format_args!("fill batch: {err}")))?;
        context.clear_kv_cache();
        context
            .decode(&mut batch)
            .map_err(|err| embed_failed(entry, format_args!("decode: {err}")))?;
        let embedding = context
            .embeddings_seq_ith(0)
            .map_err(|err| embed_failed(entry, format_args!("read sequence embedding: {err}")))?;
        vectors.push(embedding.to_vec());
    }
    truncated.sort_unstable();
    Ok(EmbeddingResult { vectors, truncated })
}

/// File name of the `hf:<repo>/<file>` model URI.
fn gguf_file_name(entry: &LlamaCppEntry) -> &str {
    match entry.uri.rsplit('/').next() {
        Some(name) if !name.is_empty() => name,
        _ => entry.uri,
    }
}

/// Splits `hf:<org>/<model>/<file>` into the `org/model` repo and the
/// remote file path (the last segment), mirroring node-llama-cpp's
/// `resolveModelFile` URI handling.
fn split_hf_uri(uri: &str) -> Option<(&str, &str)> {
    let rest = uri.strip_prefix("hf:")?;
    let (repo, file) = rest.rsplit_once('/')?;
    if repo.is_empty() || file.is_empty() || file.contains('/') {
        return None;
    }
    Some((repo, file))
}

/// Formats one text per model format, mirroring `formatTextForEmbedding`
/// exactly (both branches are frozen prompt contract).
fn format_text_for_embedding(
    text: &str,
    purpose: EmbeddingPurpose,
    format: LlamaModelFormat,
) -> String {
    match format {
        LlamaModelFormat::Qwen3 => match purpose {
            EmbeddingPurpose::Query => {
                format!("Instruct: Retrieve relevant documents for the given query\nQuery: {text}")
            }
            EmbeddingPurpose::Document => text.to_owned(),
        },
        LlamaModelFormat::Embeddinggemma => match purpose {
            EmbeddingPurpose::Query => format!("task: search result | query: {text}"),
            EmbeddingPurpose::Document => format!("title: none | text: {text}"),
        },
    }
}

/// Sniffs the cached file for the GGUF magic, mirroring
/// `validateGgufFile`: an HTML error page and any other non-GGUF content
/// are deleted and reported with their own codes.
fn validate_gguf_file(entry: &LlamaCppEntry, path: &Path) -> EngineResult<()> {
    let sniff = read_sniff(path, entry)?;
    if sniff.starts_with(GGUF_MAGIC) {
        return Ok(());
    }
    let size_kb = file_size_kb(path);
    let text = String::from_utf8_lossy(&sniff).to_lowercase();
    let is_html = text.contains("<!doctype") || text.contains("<html");
    let _ = fs::remove_file(path);
    if is_html {
        return Err(EngineError::from(ModelError::LlamaCppInvalidGgufHtml {
            reference: entry.reference.to_owned(),
            path: path.display().to_string(),
        }));
    }
    let got: String = sniff
        .iter()
        .take(GGUF_MAGIC.len())
        .flat_map(|byte| (*byte as char).escape_default())
        .collect();
    Err(EngineError::from(ModelError::LlamaCppInvalidGguf {
        reference: entry.reference.to_owned(),
        path: path.display().to_string(),
        detail: format!("expected=GGUF actual={got} sizeKB={size_kb}"),
    }))
}

/// Reads up to the sniff length for magic detection.
fn read_sniff(path: &Path, entry: &LlamaCppEntry) -> EngineResult<Vec<u8>> {
    use std::io::Read as _;
    let mut file = fs::File::open(path)
        .map_err(|err| embed_failed(entry, format_args!("open gguf: {err}")))?;
    let mut sniff = vec![0u8; GGUF_SNIFF_LEN];
    let mut filled = 0;
    while filled < GGUF_SNIFF_LEN {
        // `filled` never exceeds the buffer: reads return at most the
        // remaining capacity, so `None` is unreachable — stop regardless.
        let Some(buf) = sniff.get_mut(filled..) else {
            break;
        };
        match file.read(buf) {
            Ok(0) => break,
            Ok(read) => filled = filled.saturating_add(read),
            Err(err) => {
                return Err(embed_failed(entry, format_args!("read gguf: {err}")));
            }
        }
    }
    sniff.truncate(filled);
    Ok(sniff)
}

/// Cached file size in KiB for invalid-GGUF diagnostics.
fn file_size_kb(path: &Path) -> u64 {
    fs::metadata(path)
        .map(|meta| meta.len() / 1024)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(format: LlamaModelFormat) -> LlamaCppEntry {
        LlamaCppEntry {
            reference: "local/test-gguf",
            provider: "local",
            model: "test-gguf",
            uri: "hf:org/test-gguf-Q8_0.gguf",
            dimension: 4,
            context_size: 32,
            max_batch_size: 8,
            format,
        }
    }

    #[test]
    fn hf_uri_splits_repo_and_file() {
        assert_eq!(
            split_hf_uri("hf:ggml-org/embeddinggemma-300M-GGUF/embeddinggemma-300M-Q8_0.gguf"),
            Some((
                "ggml-org/embeddinggemma-300M-GGUF",
                "embeddinggemma-300M-Q8_0.gguf"
            ))
        );
        assert_eq!(
            split_hf_uri("hf:Qwen/Qwen3-Embedding-0.6B-GGUF/Qwen3-Embedding-0.6B-Q8_0.gguf"),
            Some((
                "Qwen/Qwen3-Embedding-0.6B-GGUF",
                "Qwen3-Embedding-0.6B-Q8_0.gguf"
            ))
        );
        assert_eq!(split_hf_uri("https://example.invalid/x.gguf"), None);
        assert_eq!(split_hf_uri("hf:noslash"), None);
        assert_eq!(split_hf_uri("hf:org/model/nested/file.gguf"), None);
    }

    #[test]
    fn prompt_formats_match_ts_contract() {
        assert_eq!(
            format_text_for_embedding("hello", EmbeddingPurpose::Query, LlamaModelFormat::Qwen3),
            "Instruct: Retrieve relevant documents for the given query\nQuery: hello"
        );
        assert_eq!(
            format_text_for_embedding("hello", EmbeddingPurpose::Document, LlamaModelFormat::Qwen3),
            "hello"
        );
        assert_eq!(
            format_text_for_embedding(
                "hello",
                EmbeddingPurpose::Query,
                LlamaModelFormat::Embeddinggemma
            ),
            "task: search result | query: hello"
        );
        assert_eq!(
            format_text_for_embedding(
                "hello",
                EmbeddingPurpose::Document,
                LlamaModelFormat::Embeddinggemma
            ),
            "title: none | text: hello"
        );
    }

    #[test]
    fn gguf_sniff_accepts_magic_and_rejects_html() {
        let dir = tempfile::tempdir().unwrap();
        let magic = dir.path().join("good.gguf");
        let mut bytes = GGUF_MAGIC.to_vec();
        bytes.extend_from_slice(&[0u8; 64]);
        fs::write(&magic, &bytes).unwrap();
        validate_gguf_file(&entry(LlamaModelFormat::Qwen3), &magic).unwrap();
        assert!(magic.exists());

        let html = dir.path().join("bad.gguf");
        fs::write(&html, b"<!doctype html><html></html>").unwrap();
        let err = validate_gguf_file(&entry(LlamaModelFormat::Qwen3), &html).unwrap_err();
        assert_eq!(
            err.code().to_string(),
            "ZVEC_GREP.ENGINE.MODELS.LLAMA_CPP_INVALID_GGUF_HTML"
        );
        assert!(!html.exists());

        let junk = dir.path().join("junk.gguf");
        fs::write(&junk, b"NOTAGGUFMODEL").unwrap();
        let err = validate_gguf_file(&entry(LlamaModelFormat::Qwen3), &junk).unwrap_err();
        assert_eq!(
            err.code().to_string(),
            "ZVEC_GREP.ENGINE.MODELS.LLAMA_CPP_INVALID_GGUF"
        );
        assert!(!junk.exists());
    }
}

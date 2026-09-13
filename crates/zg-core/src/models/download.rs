//! Model artifact downloads with aggregated progress, mirroring
//! `src/engine/models/download-progress.ts` and the downloader embedded in
//! `src/engine/models/backends/model2vec.ts`.
//!
//! [`ModelDownloadReporter`] folds per-artifact byte counts into the single
//! [`EmbeddingModelProgress`] stream the
//! engine reports. [`download_cached_file`] adds the cache-check /
//! temp-file / atomic-rename dance around [`download_file`], which streams
//! over HTTP via `ureq` without ever holding a whole artifact in memory.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use super::catalog::ModelReference;
use super::error::ModelError;
use super::{EmbeddingModelProgress, EmbeddingStageKind, ModelLoadSink};
use crate::error::{EngineError, EngineResult};

/// Progress update for one artifact download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactDownloadProgress {
    pub artifact: String,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default)]
struct ArtifactState {
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
}

/// Aggregates per-artifact download progress into one progress sink.
///
/// Lifecycle mirrors the TypeScript reporter: `start` emits `preparing`,
/// the first `begin`/`report` flips the stream into `downloading`,
/// `warning` emits a warning event (returning whether anyone listened),
/// and `finish` emits `ready`.
pub struct ModelDownloadReporter {
    sink: Option<ModelLoadSink>,
    artifacts: HashMap<String, ArtifactState>,
    download_started: bool,
}

impl ModelDownloadReporter {
    #[must_use]
    pub fn new(
        _model: &ModelReference,
        sink: Option<ModelLoadSink>,
        expected_artifacts: &[&str],
    ) -> Self {
        Self {
            sink,
            artifacts: expected_artifacts
                .iter()
                .map(|name| ((*name).to_owned(), ArtifactState::default()))
                .collect(),
            download_started: false,
        }
    }

    /// Emits the `preparing` stage.
    pub fn start(&self) {
        self.emit(EmbeddingModelProgress {
            stage: Some(EmbeddingStageKind::Preparing),
            downloaded_bytes: None,
            total_bytes: None,
            message: None,
        });
    }

    /// Marks `artifact` as actively downloading, emitting the first
    /// aggregate `downloading` event.
    pub fn begin(&mut self, artifact: &str) {
        self.artifacts.entry(artifact.to_owned()).or_default();
        if !self.download_started {
            self.download_started = true;
            self.emit_download();
        }
    }

    /// Registers an artifact that may download later, without emitting.
    pub fn register(&mut self, artifact: &str) {
        self.artifacts.entry(artifact.to_owned()).or_default();
    }

    /// Drops an already-cached artifact from the aggregate totals.
    pub fn skip(&mut self, artifact: &str) {
        self.artifacts.remove(artifact);
    }

    /// Records fresh byte counts for `artifact` and re-emits the aggregate.
    pub fn report(&mut self, progress: &ArtifactDownloadProgress) {
        self.artifacts.insert(
            progress.artifact.clone(),
            ArtifactState {
                downloaded_bytes: progress.downloaded_bytes,
                total_bytes: progress.total_bytes,
            },
        );
        self.download_started = true;
        self.emit_download();
    }

    /// Emits a warning event. Returns false when no sink listens, so the
    /// caller can fall back to stderr like the TypeScript backends do.
    #[must_use]
    pub fn warning(&self, message: &str) -> bool {
        let Some(sink) = &self.sink else {
            return false;
        };
        sink(EmbeddingModelProgress {
            stage: Some(EmbeddingStageKind::Warning),
            downloaded_bytes: None,
            total_bytes: None,
            message: Some(message.to_owned()),
        });
        true
    }

    /// Emits the `ready` stage.
    pub fn finish(&self) {
        self.emit(EmbeddingModelProgress {
            stage: Some(EmbeddingStageKind::Ready),
            downloaded_bytes: None,
            total_bytes: None,
            message: None,
        });
    }

    fn emit_download(&self) {
        let downloaded_bytes: u64 = self
            .artifacts
            .values()
            .map(|state| state.downloaded_bytes)
            .sum();
        // Totals are only meaningful when every artifact reported one.
        let complete = !self.artifacts.is_empty()
            && self
                .artifacts
                .values()
                .all(|state| state.total_bytes.is_some());
        let total_bytes: Option<u64> = if complete {
            Some(
                self.artifacts
                    .values()
                    .map(|state| state.total_bytes.unwrap_or(0))
                    .sum(),
            )
        } else {
            None
        };
        self.emit(EmbeddingModelProgress {
            stage: Some(EmbeddingStageKind::Downloading),
            downloaded_bytes: Some(downloaded_bytes),
            total_bytes,
            message: None,
        });
    }

    fn emit(&self, progress: EmbeddingModelProgress) {
        if let Some(sink) = &self.sink {
            sink(progress);
        }
    }
}

/// True when `path` exists and is non-empty.
#[must_use]
pub fn is_usable_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.len() > 0)
}

/// Builds a Hugging Face `resolve` URL for one repo file.
#[must_use]
pub fn huggingface_url(repo: &str, revision: &str, remote_file: &str) -> String {
    format!("https://huggingface.co/{repo}/resolve/{revision}/{remote_file}")
}

/// Cache location for one downloaded file: `<cache>/<scope>/<repo with / as
/// -->/<revision>/<file_name>`, mirroring the model2vec layout.
#[must_use]
pub fn scoped_cache_path(
    cache_dir: &Path,
    scope: &str,
    repo: &str,
    revision: &str,
    file_name: &str,
) -> PathBuf {
    cache_dir
        .join(scope)
        .join(repo.replace('/', "--"))
        .join(revision)
        .join(file_name)
}

/// Streams `url` to `destination` with per-chunk progress.
///
/// Writes to a sibling temp file first and atomically renames on success;
/// a failed download never leaves a partial file at `destination`.
///
/// # Errors
///
/// Returns [`ModelError::DownloadFailed`] when the cache directory cannot be created, the request
/// or stream fails, the downloaded file is empty, or the rename into place fails.
pub fn download_file(
    url: &str,
    destination: &Path,
    on_progress: Option<&mut dyn FnMut(u64, Option<u64>)>,
) -> EngineResult<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            EngineError::from(ModelError::DownloadFailed {
                context: format!("url={url} detail=create cache dir: {err}"),
            })
        })?;
    }
    let temp_path = destination.with_extension(format!("part-{}", std::process::id()));
    let result = download_to(&temp_path, url, on_progress);
    match result {
        Ok(()) => {
            if !is_usable_file(&temp_path) {
                let _ = fs::remove_file(&temp_path);
                return Err(EngineError::from(ModelError::DownloadFailed {
                    context: format!("url={url} detail=downloaded file is empty"),
                }));
            }
            fs::rename(&temp_path, destination).map_err(|err| {
                EngineError::from(ModelError::DownloadFailed {
                    context: format!("url={url} detail=rename into cache: {err}"),
                })
            })
        }
        Err(err) => {
            let _ = fs::remove_file(&temp_path);
            Err(err)
        }
    }
}

/// Ensures `remote_file` from a Hugging Face repo is present at `local_path`,
/// downloading it when the cache misses. Reports through `reporter` under
/// `artifact_name` (usually the file's base name).
///
/// # Errors
///
/// Returns [`ModelError::DownloadFailed`] when the cached download fails.
pub fn download_cached_file(
    repo: &str,
    revision: &str,
    remote_file: &str,
    local_path: &Path,
    artifact_name: &str,
    reporter: &mut ModelDownloadReporter,
) -> EngineResult<PathBuf> {
    if is_usable_file(local_path) {
        reporter.skip(artifact_name);
        return Ok(local_path.to_owned());
    }
    let url = huggingface_url(repo, revision, remote_file);
    reporter.begin(artifact_name);
    let artifact = artifact_name.to_owned();
    let result = download_file(
        &url,
        local_path,
        Some(&mut |downloaded, total| {
            reporter.report(&ArtifactDownloadProgress {
                artifact: artifact.clone(),
                downloaded_bytes: downloaded,
                total_bytes: total,
            });
        }),
    );
    if let Err(err) = result {
        return Err(EngineError::from(ModelError::DownloadFailed {
            context: format!("repo={repo} revision={revision} detail={err}"),
        }));
    }
    Ok(local_path.to_owned())
}

fn download_to(
    temp_path: &Path,
    url: &str,
    on_progress: Option<&mut dyn FnMut(u64, Option<u64>)>,
) -> EngineResult<()> {
    let mut response = ureq::get(url).call().map_err(|err| {
        EngineError::from(ModelError::DownloadFailed {
            context: format!("url={url} detail={err}"),
        })
    })?;
    let total_bytes: Option<u64> = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|text| text.parse().ok());
    let mut reader = ProgressReader {
        inner: response.body_mut().as_reader(),
        downloaded_bytes: 0,
        total_bytes,
        on_progress,
    };
    let mut file = fs::File::create(temp_path).map_err(|err| {
        EngineError::from(ModelError::DownloadFailed {
            context: format!("url={url} detail=create temp file: {err}"),
        })
    })?;
    std::io::copy(&mut reader, &mut file).map_err(|err| {
        EngineError::from(ModelError::DownloadFailed {
            context: format!("url={url} detail=stream body: {err}"),
        })
    })?;
    file.flush().map_err(|err| {
        EngineError::from(ModelError::DownloadFailed {
            context: format!("url={url} detail=flush file: {err}"),
        })
    })?;
    Ok(())
}

struct ProgressReader<'a, R> {
    inner: R,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    on_progress: Option<&'a mut dyn FnMut(u64, Option<u64>)>,
}

impl<R: Read> Read for ProgressReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.downloaded_bytes += read as u64;
        if let Some(callback) = &mut self.on_progress {
            callback(self.downloaded_bytes, self.total_bytes);
        }
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn reporter_with_sink(
        expected: &[&str],
    ) -> (
        ModelDownloadReporter,
        Arc<Mutex<Vec<EmbeddingModelProgress>>>,
    ) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink_events = Arc::clone(&events);
        let sink: ModelLoadSink = Arc::new(move |progress| {
            if let Ok(mut guard) = sink_events.lock() {
                guard.push(progress);
            }
        });
        let reporter = ModelDownloadReporter::new(
            &ModelReference::from("local/potion-retrieval-32m"),
            Some(sink),
            expected,
        );
        (reporter, events)
    }

    fn last_event(events: &Arc<Mutex<Vec<EmbeddingModelProgress>>>) -> EmbeddingModelProgress {
        events
            .lock()
            .ok()
            .and_then(|guard| guard.last().cloned())
            .unwrap_or_default()
    }

    #[test]
    fn totals_require_every_artifact() {
        let (mut reporter, events) = reporter_with_sink(&["model.safetensors", "tokenizer.json"]);
        reporter.start();
        reporter.report(&ArtifactDownloadProgress {
            artifact: "model.safetensors".to_owned(),
            downloaded_bytes: 100,
            total_bytes: Some(200),
        });
        // tokenizer.json has no total yet: aggregate must omit the total.
        let last = last_event(&events);
        assert_eq!(last.downloaded_bytes, Some(100));
        assert_eq!(last.total_bytes, None);
        reporter.report(&ArtifactDownloadProgress {
            artifact: "tokenizer.json".to_owned(),
            downloaded_bytes: 50,
            total_bytes: Some(50),
        });
        let last = last_event(&events);
        assert_eq!(last.downloaded_bytes, Some(150));
        assert_eq!(last.total_bytes, Some(250));
    }

    #[test]
    fn warning_without_sink_returns_false() {
        let reporter = ModelDownloadReporter::new(
            &ModelReference::from("local/potion-retrieval-32m"),
            None,
            &[],
        );
        assert!(!reporter.warning("boom"));
    }

    #[test]
    fn cache_path_flattens_repo_slashes() {
        let path = scoped_cache_path(
            Path::new("/cache"),
            "model2vec",
            "minishlab/potion-retrieval-32M",
            "rev",
            "model.safetensors",
        );
        assert_eq!(
            path,
            PathBuf::from("/cache/model2vec/minishlab--potion-retrieval-32M/rev/model.safetensors")
        );
    }
}

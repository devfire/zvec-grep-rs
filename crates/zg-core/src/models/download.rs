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
use std::time::Duration;

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
/// Hard ceiling for one model artifact download (8 GiB): bounds the
/// `Content-Length` precheck and the streaming copy alike, so an
/// unbounded response can never fill the disk.
const MAX_ARTIFACT_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Connection, first-byte, body, and global budgets for artifact fetch.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
const BODY_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const GLOBAL_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Redirect hops followed per artifact fetch; every hop is re-validated
/// against the endpoint policy and the host allowlist below.
const MAX_REDIRECTS: u32 = 5;
/// Random temp-file creation attempts before surfacing a collision error.
const TEMP_CREATE_ATTEMPTS: usize = 5;

/// Builds an artifact-fetch agent: redirects are followed manually (so
/// every hop is re-validated) and every call is bounded by
/// connect/first-byte/body/global timeouts.
fn download_agent() -> ureq::Agent {
    let config = ureq::config::Config::builder()
        .timeout_global(Some(GLOBAL_TIMEOUT))
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(RESPONSE_TIMEOUT))
        .timeout_recv_body(Some(BODY_TIMEOUT))
        .http_status_as_error(false)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .build();
    ureq::Agent::new_with_config(config)
}

/// True when `host` may serve model artifacts: `huggingface.co` itself or
/// a sub-domain of `huggingface.co` / `hf.co` (the LFS and XetHub CDN
/// redirect targets live there).
fn is_allowed_artifact_host(host: &str) -> bool {
    let lower = host.to_lowercase();
    lower == "huggingface.co"
        || lower == "hf.co"
        || lower.ends_with(".huggingface.co")
        || lower.ends_with(".hf.co")
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
    if let Some(parent) = destination.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|err| {
            EngineError::from(ModelError::DownloadFailed {
                context: format!("url={url} detail=create cache dir: {err}"),
            })
        })?;
    }
    let (temp_path, file) = create_temp_file(destination, url)?;
    let result = download_to(url, file, on_progress);
    match result {
        Ok(()) => {
            if !is_usable_file(&temp_path) {
                let _ = fs::remove_file(&temp_path);
                return Err(EngineError::from(ModelError::DownloadFailed {
                    context: format!("url={url} detail=downloaded file is empty"),
                }));
            }
            if fs::symlink_metadata(&temp_path).is_ok_and(|meta| meta.file_type().is_symlink()) {
                let _ = fs::remove_file(&temp_path);
                return Err(EngineError::from(ModelError::DownloadFailed {
                    context: format!("url={url} detail=temp file was replaced by a symlink"),
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

/// Creates an unpredictable sibling temp file for `destination`: a
/// dot-prefixed `<name>.part-<uuid>.tmp` opened with `create_new`, so a
/// pre-placed symlink at a guessable path can never steer the download
/// write. Retries on collision instead of following anything.
fn create_temp_file(destination: &Path, url: &str) -> EngineResult<(PathBuf, fs::File)> {
    let failed = |detail: String| {
        EngineError::from(ModelError::DownloadFailed {
            context: format!("url={url} detail={detail}"),
        })
    };
    let dir: &Path = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let stem = destination
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    for _ in 0..TEMP_CREATE_ATTEMPTS {
        let candidate = dir.join(format!(
            ".{stem}.part-{}.tmp",
            uuid::Uuid::new_v4().simple()
        ));
        if fs::symlink_metadata(&candidate).is_ok_and(|meta| meta.file_type().is_symlink()) {
            continue;
        }
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(failed(format!("create temp file: {err}"))),
        }
    }
    Err(failed("create temp file: too many collisions".to_owned()))
}

/// Rejects artifact URLs outside the endpoint policy or the artifact host
/// allowlist. Runs on the initial URL and on every redirect hop.
fn validate_artifact_url(candidate: &str, url: &str) -> EngineResult<()> {
    if !crate::config::is_http_endpoint(candidate) {
        return Err(EngineError::from(ModelError::DownloadFailed {
            context: format!("url={url} detail=blocked artifact endpoint"),
        }));
    }
    let host_ok = url::Url::parse(candidate)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_owned))
        .is_some_and(|host| is_allowed_artifact_host(&host));
    if !host_ok {
        return Err(EngineError::from(ModelError::DownloadFailed {
            context: format!("url={url} detail=artifact host is not allowlisted"),
        }));
    }
    Ok(())
}

/// Resolves a redirect `location` against the current URL, refusing
/// `https` -> `http` downgrades.
fn join_redirect(current: &str, location: &str, url: &str) -> EngineResult<String> {
    let failed = |detail: &str| {
        EngineError::from(ModelError::DownloadFailed {
            context: format!("url={url} detail={detail}"),
        })
    };
    if location.is_empty() {
        return Err(failed("redirect without location"));
    }
    let Ok(base) = url::Url::parse(current) else {
        return Err(failed("redirect from invalid URL"));
    };
    let Ok(next) = base.join(location) else {
        return Err(failed("redirect to invalid location"));
    };
    if base.scheme() == "https" && next.scheme() == "http" {
        return Err(failed("refusing https-to-http redirect"));
    }
    Ok(next.to_string())
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
    url: &str,
    mut file: fs::File,
    on_progress: Option<&mut dyn FnMut(u64, Option<u64>)>,
) -> EngineResult<()> {
    let agent = download_agent();
    let mut current = url.to_owned();
    for _ in 0..=MAX_REDIRECTS {
        validate_artifact_url(&current, url)?;
        let mut response = agent.get(&current).call().map_err(|err| {
            EngineError::from(ModelError::DownloadFailed {
                context: format!("url={url} detail={err}"),
            })
        })?;
        let status = response.status().as_u16();
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            let location = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_owned();
            current = join_redirect(&current, &location, url)?;
            continue;
        }
        if !(200..300).contains(&status) {
            return Err(EngineError::from(ModelError::DownloadFailed {
                context: format!("url={url} detail=unexpected status {status}"),
            }));
        }
        let total_bytes: Option<u64> = response
            .headers()
            .get("content-length")
            .and_then(|value| value.to_str().ok())
            .and_then(|text| text.parse().ok());
        if total_bytes.is_some_and(|total| total > MAX_ARTIFACT_BYTES) {
            return Err(EngineError::from(ModelError::DownloadFailed {
                context: format!(
                    "url={url} detail=response exceeds size limit ({MAX_ARTIFACT_BYTES} bytes)"
                ),
            }));
        }
        let mut reader = ProgressReader {
            inner: response.body_mut().as_reader(),
            downloaded_bytes: 0,
            total_bytes,
            on_progress,
        };
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
        return Ok(());
    }
    Err(EngineError::from(ModelError::DownloadFailed {
        context: format!("url={url} detail=too many redirects"),
    }))
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
        self.downloaded_bytes = self.downloaded_bytes.saturating_add(read as u64);
        if self.downloaded_bytes > MAX_ARTIFACT_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                "artifact exceeds size limit",
            ));
        }
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

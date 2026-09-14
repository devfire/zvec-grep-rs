//! Embedding retry and adaptive scheduling: backoff, fail-fast classification, semaphore.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crate::error::{EngineError, EngineErrorCode, EngineResult};
use crate::models::embeddings::EmbeddingResult;
use crate::models::{EmbeddingInput, EmbeddingModel, EmbeddingPurpose, ModelLoadSink};
use crate::types::{Content, IndexEmbeddingProgress};

use super::context::{
    EMBEDDING_RATE_LIMIT_MAX_RETRIES, EMBEDDING_RATE_LIMIT_RETRY_BASE_DELAY_MS,
    EMBEDDING_RATE_LIMIT_RETRY_MAX_DELAY_MS, EMBEDDING_RETRY_JITTER_MS,
    EMBEDDING_SUCCESS_STREAK_MIN, EMBEDDING_TRANSIENT_MAX_RETRIES,
    EMBEDDING_TRANSIENT_RETRY_BASE_DELAY_MS, EMBEDDING_TRANSIENT_RETRY_MAX_DELAY_MS,
    PERMANENT_REMOTE_MODEL_PROVIDER_CODES, is_cancelled_or_aborted, throw_if_aborted,
};
use super::scanner::CancelFlag;

pub(crate) fn embed_contents_with_retry(
    contents: &[Content],
    model: &dyn EmbeddingModel,
    scheduler: &EmbeddingScheduler,
    abort: &AtomicBool,
    cancel: Option<&CancelFlag>,
    on_model_progress: Option<ModelLoadSink>,
    on_terminal_failure: Option<&AtomicBool>,
) -> EngineResult<EmbeddingResult> {
    let _ = on_model_progress;
    let mut attempt: u32 = 0;
    loop {
        throw_if_aborted(abort, cancel)?;
        let outcome = scheduler.run(abort, cancel, |abort, cancel| {
            throw_if_aborted(abort, cancel)?;
            let inputs: Vec<EmbeddingInput<'_>> = contents.iter().map(content_to_input).collect();
            model.embed(EmbeddingPurpose::Document, &inputs)
        });
        match outcome {
            Ok(result) => {
                scheduler.record_success();
                return Ok(result);
            }
            Err(error) => {
                if is_cancelled_or_aborted(&error) {
                    return Err(error);
                }
                let retry = classify_embedding_retry(&error, model);
                let delay_ms = retry_delay_ms(attempt, &retry);
                if retry.retryable {
                    scheduler.record_retryable_failure(EmbeddingRetryDecision {
                        rate_limited: retry.rate_limited,
                        delay_ms,
                    });
                }
                if retry.fail_fast
                    && (!retry.retryable || attempt >= max_retry_attempts(&retry))
                    && let Some(flag) = on_terminal_failure
                {
                    flag.store(true, Ordering::Relaxed);
                }
                if attempt >= max_retry_attempts(&retry) || !retry.retryable {
                    return Err(error);
                }
                abortable_sleep(delay_ms, abort, cancel)?;
                attempt += 1;
            }
        }
    }
}

fn content_to_input(content: &Content) -> EmbeddingInput<'_> {
    match content {
        Content::Text { text } => EmbeddingInput::Text { text },
        Content::Image { data, format } => EmbeddingInput::Image {
            data,
            format: *format,
        },
    }
}

fn abortable_sleep(ms: u64, abort: &AtomicBool, cancel: Option<&CancelFlag>) -> EngineResult<()> {
    if ms == 0 {
        return throw_if_aborted(abort, cancel);
    }
    let deadline = Instant::now() + Duration::from_millis(ms);
    loop {
        throw_if_aborted(abort, cancel)?;
        if Instant::now() >= deadline {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ConcurrencyPolicy {
    pub initial: usize,
    pub min: usize,
    pub max: usize,
    pub adaptive: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct EmbeddingRetryDecision {
    pub rate_limited: bool,
    pub delay_ms: u64,
}

#[derive(Debug, Clone)]
struct RetryClassification {
    retryable: bool,
    rate_limited: bool,
    fail_fast: bool,
    retry_after_ms: Option<u64>,
}

struct SchedulerState {
    active: usize,
    current: usize,
    cooldown_until: Option<Instant>,
    retryable_failures: usize,
    success_streak: usize,
}

/// Bounded adaptive semaphore around embedding calls (mirrors
/// `AdaptiveEmbeddingScheduler`).
pub struct EmbeddingScheduler {
    policy: ConcurrencyPolicy,
    state: Mutex<SchedulerState>,
    cvar: Condvar,
}

impl EmbeddingScheduler {
    #[must_use]
    pub fn new(policy: ConcurrencyPolicy) -> Self {
        let initial = policy.initial;
        Self {
            policy,
            state: Mutex::new(SchedulerState {
                active: 0,
                current: initial,
                cooldown_until: None,
                retryable_failures: 0,
                success_streak: 0,
            }),
            cvar: Condvar::new(),
        }
    }

    pub fn policy(&self) -> ConcurrencyPolicy {
        self.policy
    }

    pub fn task_concurrency(&self) -> usize {
        self.policy.max
    }

    /// Runs `task` under the concurrency and cooldown gates, releasing the slot afterwards.
    ///
    /// # Errors
    ///
    /// Returns `INDEXING.CANCELLED` when aborted or cancelled, `INDEXING.SCHEDULER_FAILED`
    /// when the scheduler lock fails, or the error returned by `task`.
    pub fn run<T>(
        &self,
        abort: &AtomicBool,
        cancel: Option<&CancelFlag>,
        task: impl FnOnce(&AtomicBool, Option<&CancelFlag>) -> EngineResult<T>,
    ) -> EngineResult<T> {
        throw_if_aborted(abort, cancel)?;
        self.wait_for_cooldown(abort, cancel)?;
        self.acquire(abort, cancel)?;
        self.wait_for_cooldown(abort, cancel)?;
        let result = task(abort, cancel);
        self.release();
        result
    }

    pub fn record_success(&self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if !self.policy.adaptive || state.current >= self.policy.max {
            return;
        }
        state.success_streak += 1;
        if state.success_streak < EMBEDDING_SUCCESS_STREAK_MIN.max(state.current * 2) {
            return;
        }
        state.current += 1;
        state.success_streak = 0;
        self.cvar.notify_all();
    }

    pub fn record_retryable_failure(&self, retry: EmbeddingRetryDecision) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.retryable_failures += 1;
        if retry.rate_limited && retry.delay_ms > 0 {
            let until = Instant::now() + Duration::from_millis(retry.delay_ms);
            state.cooldown_until = Some(
                state
                    .cooldown_until
                    .map_or(until, |current| current.max(until)),
            );
        }
        if !self.policy.adaptive {
            return;
        }
        state.current = self.policy.min.max(state.current / 2);
        state.success_streak = 0;
    }

    pub fn snapshot(&self) -> IndexEmbeddingProgress {
        let (current, max, retryable_failures) = match self.state.lock() {
            Ok(state) => (state.current, self.policy.max, state.retryable_failures),
            Err(_) => (self.policy.initial, self.policy.max, 0),
        };
        IndexEmbeddingProgress {
            concurrency: Some(current),
            max_concurrency: Some(max),
            retryable_failures: Some(retryable_failures),
            ..IndexEmbeddingProgress::default()
        }
    }

    fn wait_for_cooldown(
        &self,
        abort: &AtomicBool,
        cancel: Option<&CancelFlag>,
    ) -> EngineResult<()> {
        loop {
            throw_if_aborted(abort, cancel)?;
            let remaining = match self.state.lock() {
                Ok(state) => state
                    .cooldown_until
                    .map(|until| until.saturating_duration_since(Instant::now())),
                Err(_) => None,
            };
            match remaining {
                Some(duration) if !duration.is_zero() => {
                    std::thread::sleep(duration.min(Duration::from_millis(50)));
                }
                _ => return Ok(()),
            }
        }
    }

    fn acquire(&self, abort: &AtomicBool, cancel: Option<&CancelFlag>) -> EngineResult<()> {
        let mut state = self.state.lock().map_err(|_| {
            EngineError::new(
                EngineErrorCode::from_static("INDEXING.SCHEDULER_FAILED"),
                "embedding scheduler lock failed",
            )
        })?;
        loop {
            throw_if_aborted(abort, cancel)?;
            if state.active < state.current {
                state.active += 1;
                return Ok(());
            }
            state = self
                .cvar
                .wait_timeout(state, Duration::from_millis(50))
                .map_err(|_| {
                    EngineError::new(
                        EngineErrorCode::from_static("INDEXING.SCHEDULER_FAILED"),
                        "embedding scheduler lock failed",
                    )
                })?
                .0;
        }
    }

    fn release(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.active = state.active.saturating_sub(1);
        }
        self.cvar.notify_all();
    }
}

pub(crate) fn resolve_embedding_concurrency_policy(
    requested: Option<usize>,
    model: &dyn EmbeddingModel,
) -> ConcurrencyPolicy {
    if let Some(requested) = requested.filter(|value| *value > 0) {
        return ConcurrencyPolicy {
            initial: requested,
            min: 1,
            max: requested,
            adaptive: requested > 1,
        };
    }
    let info = model.info();
    let remote = info.provider != "local";
    let multimodal = info.supports_images;
    let local_default = info
        .default_concurrency
        .filter(|value| *value > 0)
        .unwrap_or(1);
    let initial = if remote {
        if multimodal { 4 } else { 8 }
    } else {
        local_default
    };
    let max = if remote {
        if multimodal { 8 } else { 12 }
    } else {
        local_default
    };
    let min = initial.min(4);
    ConcurrencyPolicy {
        initial,
        min,
        max: initial.max(max),
        adaptive: max > 1,
    }
}

pub(crate) fn should_fail_fast_embedding_error(
    error: &EngineError,
    model: &dyn EmbeddingModel,
) -> bool {
    classify_embedding_retry(error, model).fail_fast
}

fn classify_embedding_retry(
    error: &EngineError,
    model: &dyn EmbeddingModel,
) -> RetryClassification {
    let code = error.code().qualified();
    let text = format!(
        "{} {} {}",
        code,
        error.message(),
        error.context().unwrap_or("")
    );
    let status = http_status_from_text(&text);
    let remote = model.info().provider != "local";
    let rate_limited = remote
        && (status == Some(429)
            || contains_insensitive(&text, "rate limit")
            || contains_insensitive(&text, "quota exceeded")
            || contains_insensitive(&text, "too many requests")
            || contains_insensitive(&text, "request rate increased too quickly"));
    let server_error = remote && status.is_some_and(|status| (500..600).contains(&status));
    let request_timeout = remote && status == Some(408);
    let request_failure = code.ends_with("_REQUEST_FAILED");
    let transient_network = remote && request_failure && is_transient_network_failure(&text);
    let shared_local_failure = matches!(
        code.as_str(),
        "ZVEC_GREP.ENGINE.MODELS.MODEL2VEC_DOWNLOAD_FAILED"
            | "ZVEC_GREP.ENGINE.MODELS.MODEL2VEC_LOAD_FAILED"
            | "ZVEC_GREP.ENGINE.MODELS.MODEL2VEC_DISPOSED"
    );
    let retryable = !shared_local_failure
        && (rate_limited || server_error || request_timeout || transient_network);
    let remote_configuration_failure = remote
        && (code.ends_with("_MISSING_API_KEY")
            || code.ends_with("_MISSING_ENDPOINT")
            || status == Some(401)
            || status == Some(403)
            || status == Some(404)
            || contains_insensitive(&text, "api key"));
    let permanent_remote = remote
        && (code == "ZVEC_GREP.ENGINE.MODELS.EMBEDDING_DIMENSION_MISMATCH"
            || (status == Some(400) && is_permanent_remote_model_bad_request(&text)));
    RetryClassification {
        retryable,
        rate_limited,
        fail_fast: retryable
            || shared_local_failure
            || remote_configuration_failure
            || permanent_remote
            || (remote && request_failure),
        retry_after_ms: retry_after_ms_from_text(&text),
    }
}

fn is_permanent_remote_model_bad_request(text: &str) -> bool {
    if let Some(code) = find_key_value(text, "providerCode=") {
        let normalized: String = code
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() {
                    ch.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect();
        let normalized = normalized.trim_matches('_');
        if PERMANENT_REMOTE_MODEL_PROVIDER_CODES.contains(&normalized) {
            return true;
        }
    }
    match find_provider_message(text) {
        Some(message) => {
            let lower = message.to_lowercase();
            (contains_word(&lower, "invalid")
                || contains_word(&lower, "unsupported")
                || contains_word(&lower, "unknown"))
                && contains_word(&lower, "model")
        }
        None => false,
    }
}

fn find_key_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let position = text.to_lowercase().find(&key.to_lowercase())?;
    text[position + key.len()..].split_whitespace().next()
}

fn find_provider_message(text: &str) -> Option<String> {
    let key = "providermessage=";
    let position = text.to_lowercase().find(key)?;
    let start = position + key.len();
    let end = text.to_lowercase()[start..]
        .find("zvec_grep.")
        .map(|offset| start + offset)
        .unwrap_or(text.len());
    Some(text[start..end].to_owned())
}

fn contains_word(haystack: &str, word: &str) -> bool {
    haystack
        .split(|ch: char| !ch.is_alphanumeric())
        .any(|part| part == word)
}

fn contains_insensitive(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

fn is_transient_network_failure(text: &str) -> bool {
    const MARKERS: &[&str] = &[
        "EAI_AGAIN",
        "ECONNREFUSED",
        "ECONNRESET",
        "EHOSTUNREACH",
        "ENETDOWN",
        "ENETUNREACH",
        "ENOTFOUND",
        "ETIMEDOUT",
        "UND_ERR_CONNECT_TIMEOUT",
        "UND_ERR_HEADERS_TIMEOUT",
        "UND_ERR_SOCKET",
        "TimeoutError",
        "socket hang up",
        "temporary failure",
    ];
    MARKERS
        .iter()
        .any(|marker| contains_insensitive(text, marker))
        || contains_insensitive(text, "connection reset")
        || contains_insensitive(text, "connection timed out")
        || contains_insensitive(text, "network connection failed")
        || contains_insensitive(text, "network connection was lost")
}

fn max_retry_attempts(retry: &RetryClassification) -> u32 {
    if retry.rate_limited {
        EMBEDDING_RATE_LIMIT_MAX_RETRIES
    } else {
        EMBEDDING_TRANSIENT_MAX_RETRIES
    }
}

fn retry_delay_ms(attempt: u32, retry: &RetryClassification) -> u64 {
    if let Some(retry_after) = retry.retry_after_ms {
        return retry_after;
    }
    let (base, max) = if retry.rate_limited {
        (
            EMBEDDING_RATE_LIMIT_RETRY_BASE_DELAY_MS,
            EMBEDDING_RATE_LIMIT_RETRY_MAX_DELAY_MS,
        )
    } else {
        (
            EMBEDDING_TRANSIENT_RETRY_BASE_DELAY_MS,
            EMBEDDING_TRANSIENT_RETRY_MAX_DELAY_MS,
        )
    };
    let exponential = base.saturating_mul(1u64 << attempt.min(20));
    exponential.saturating_add(jitter_ms()).min(max)
}

fn jitter_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| u64::from(duration.subsec_nanos()) % (EMBEDDING_RETRY_JITTER_MS + 1))
        .unwrap_or(0)
}

fn http_status_from_text(text: &str) -> Option<u16> {
    find_key_value(text, "status=").and_then(|value| value.parse().ok())
}

fn retry_after_ms_from_text(text: &str) -> Option<u64> {
    if let Some(ms) = find_key_value(text, "retryAfterMs=")
        && let Ok(value) = ms.parse()
    {
        return Some(value);
    }
    find_key_value(text, "retryAfter=")
        .and_then(|value| value.parse::<f64>().ok())
        .map(|seconds| (seconds * 1000.0).round() as u64)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn scheduler_serializes_permits() {
        let scheduler = EmbeddingScheduler::new(ConcurrencyPolicy {
            initial: 1,
            min: 1,
            max: 1,
            adaptive: false,
        });
        let abort = AtomicBool::new(false);
        let abort_ref = &abort;
        let scheduler_ref = &scheduler;
        let counter = Arc::new(Mutex::new(0usize));
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let counter = Arc::clone(&counter);
                scope.spawn(move || {
                    scheduler_ref
                        .run(abort_ref, None, |_, _| {
                            *counter
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner()) += 1;
                            Ok::<_, EngineError>(())
                        })
                        .expect("run");
                });
            }
        });
        assert_eq!(
            *counter
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            4
        );
    }

    #[test]
    fn transient_retryable_then_gives_up() {
        struct Failing;
        impl EmbeddingModel for Failing {
            fn info(&self) -> &crate::models::EmbeddingModelInfo {
                use std::sync::OnceLock;
                static INFO: OnceLock<crate::models::EmbeddingModelInfo> = OnceLock::new();
                INFO.get_or_init(|| crate::models::EmbeddingModelInfo {
                    reference: "test/failing".to_owned(),
                    provider: "qwen".to_owned(),
                    model: "test-model".to_owned(),
                    dimension: 4,
                    metric: crate::types::SearchMetric::Cosine,
                    supports_images: false,
                    max_input_tokens: None,
                    endpoint: None,
                    input_kinds: vec![crate::models::EmbeddingInputKind::Text],
                    default_concurrency: None,
                })
            }
            fn max_batch_size(&self) -> usize {
                8
            }
            fn embed(
                &self,
                _purpose: EmbeddingPurpose,
                _inputs: &[EmbeddingInput<'_>],
            ) -> EngineResult<EmbeddingResult> {
                Err(EngineError::new(
                    EngineErrorCode::from_static("MODELS.QWEN_TEXT_EMBEDDING_REQUEST_FAILED"),
                    "request failed",
                )
                .with_context("status=503"))
            }
        }
        let model: Arc<dyn EmbeddingModel> = Arc::new(Failing);
        let scheduler =
            EmbeddingScheduler::new(resolve_embedding_concurrency_policy(None, &*model));
        let abort = AtomicBool::new(false);
        let contents = vec![Content::Text {
            text: "hello".to_owned(),
        }];
        let result =
            embed_contents_with_retry(&contents, &*model, &scheduler, &abort, None, None, None);
        assert!(result.is_err());
        // 1 initial + 3 retries for transient failures.
        assert!(scheduler.snapshot().retryable_failures.unwrap_or(0) >= 3);
    }
}

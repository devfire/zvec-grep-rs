//! Qwen remote embedding backends.
//!
//! Mirrors `src/engine/models/backends/qwen.ts`: text models speak the
//! OpenAI-compatible embeddings API (`POST {model, input, dimensions,
//! encoding_format}` returning `{data: [{index, embedding}]}`), while
//! `qwen3-vl-embedding` speaks the DashScope multimodal API (`POST
//! {model, input: {contents}, parameters: {dimension}}` returning
//! `{output: {embeddings: [{index | text_index, embedding}]}}`). Requests
//! are synchronous via `ureq` with `Bearer` API-key auth and a 60 s global
//! timeout, matching `DEFAULT_REMOTE_EMBEDDING_TIMEOUT_MS`.
//!
//! The provider always returns full batches with no truncation metadata, so
//! `truncated` is empty; dimension and finiteness checks run through the
//! shared [`validate_result`](crate::models::embeddings::validate_result)
//! discipline like the TypeScript `validateResult`.

use std::time::Duration;

use serde_json::Value;

use crate::authorization::error::RemoteEmbeddingPurpose;
use crate::authorization::operation::RemoteEmbeddingGuard;
use crate::authorization::types::{ContentKind, RemoteEmbeddingRequest};
use crate::error::{EngineError, EngineErrorCode, EngineResult};
use crate::models::catalog::{QwenMultimodalEntry, QwenTextEntry};
use crate::models::embeddings::{ApiKey, EmbeddingResult, embed_validated};
use crate::models::error::{
    ModelError, QwenTextFailure, QwenTextModel, QwenVlFailure, qwen_vl_code,
};
use crate::models::{
    EmbeddingInput, EmbeddingInputKind, EmbeddingModel, EmbeddingModelInfo, EmbeddingPurpose,
};
use crate::types::{ImageFormat, SearchMetric};

/// Remote embedding request timeout, mirroring
/// `DEFAULT_REMOTE_EMBEDDING_TIMEOUT_MS`.
const REMOTE_TIMEOUT_MS: u64 = 60_000;
const REMOTE_TIMEOUT: Duration = Duration::from_millis(REMOTE_TIMEOUT_MS);
/// Maximum images per Qwen3 VL embedding request, mirroring
/// `QWEN3_VL_EMBEDDING_MAX_IMAGE_COUNT`.
const VL_MAX_IMAGE_COUNT: usize = 10;
/// Builds the shared remote agent: HTTP errors surface as responses (so the
/// provider body can be parsed) and every call is bounded by the global
/// timeout, mirroring `remoteEmbeddingSignal`.
fn remote_agent() -> ureq::Agent {
    let config = ureq::config::Config::builder()
        .timeout_global(Some(REMOTE_TIMEOUT))
        .http_status_as_error(false)
        .build();
    ureq::Agent::new_with_config(config)
}

/// Sends one JSON embedding request with `Bearer` auth, returning the parsed
/// body. Transport, JSON, and provider failures carry their own codes,
/// mirroring the three TypeScript throw sites.
#[allow(clippy::too_many_arguments)]
fn post_embedding_json(
    agent: &ureq::Agent,
    display: &str,
    reference: &str,
    endpoint: &str,
    api_key: &ApiKey,
    body: Value,
    request_failed: EngineErrorCode,
    invalid_json: EngineErrorCode,
    api_error: EngineErrorCode,
) -> EngineResult<Value> {
    let mut response = agent
        .post(endpoint)
        .header("Authorization", &format!("Bearer {}", api_key.as_str()))
        .send_json(body)
        .map_err(|err| {
            EngineError::new(request_failed, format!("{display} request failed")).with_context(
                format!("model={reference} endpoint={endpoint} timeoutMs={REMOTE_TIMEOUT_MS} detail={err}"),
            )
        })?;
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(parse_retry_after_ms);
    let parsed: Value = response.body_mut().read_json().map_err(|err| {
        EngineError::new(
            invalid_json,
            format!("{display} response was not valid JSON"),
        )
        .with_context(format!("model={reference} status={status} detail={err}"))
    })?;
    if !(200..300).contains(&status) {
        let error = read_provider_error(&parsed);
        return Err(
            EngineError::new(api_error, format!("{display} request returned an error"))
                .with_context(provider_error_context(
                    reference,
                    status,
                    retry_after,
                    &error,
                )),
        );
    }
    Ok(parsed)
}

/// Extracts `{code, type, message}` from a provider error body, accepting
/// either `{error: {...}}` or a flat `{code, message}` shape.
struct ProviderError {
    code: String,
    error_type: String,
    message: String,
}

fn read_provider_error(body: &Value) -> ProviderError {
    if let Some(error) = body.get("error").and_then(Value::as_object) {
        return ProviderError {
            code: error
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
            error_type: error
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
        };
    }
    if let Some(object) = body.as_object() {
        return ProviderError {
            code: object
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
            error_type: "unknown".to_owned(),
            message: object
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
        };
    }
    ProviderError {
        code: "unknown".to_owned(),
        error_type: "unknown".to_owned(),
        message: "unknown".to_owned(),
    }
}

fn provider_error_context(
    reference: &str,
    status: u16,
    retry_after_ms: Option<u64>,
    error: &ProviderError,
) -> String {
    let retry = retry_after_ms
        .map(|ms| format!(" retryAfterMs={ms}"))
        .unwrap_or_default();
    format!(
        "model={reference} status={status}{retry} providerCode={} providerType={} providerMessage={}",
        error.code, error.error_type, error.message
    )
}

/// Parses a `retry-after` header: seconds first, else an HTTP date relative
/// to now, mirroring `retryAfterHeaderMs`.
fn parse_retry_after_ms(value: &str) -> Option<u64> {
    let trimmed = value.trim();
    if let Ok(seconds) = trimmed.parse::<f64>()
        && seconds.is_finite()
        && seconds >= 0.0
    {
        return Some((seconds * 1000.0).round() as u64);
    }
    let date = chrono::NaiveDateTime::parse_from_str(trimmed, "%a, %d %b %Y %H:%M:%S GMT").ok()?;
    let target = date.and_utc().timestamp_millis();
    let now = chrono::Utc::now().timestamp_millis();
    Some(target.saturating_sub(now).max(0) as u64)
}

/// Reads one embedding vector, rejecting non-array payloads and non-numeric
/// components with `INVALID_VECTOR`.
fn read_vector(
    code: EngineErrorCode,
    display: &str,
    reference: &str,
    index: usize,
    item: &Value,
) -> EngineResult<Vec<f32>> {
    let raw = item
        .get("embedding")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            EngineError::new(
                code,
                format!("{display} response included an invalid embedding"),
            )
            .with_context(format!("model={reference} index={index}"))
        })?;
    let mut vector = Vec::with_capacity(raw.len());
    for value in raw {
        let component = value.as_f64().ok_or_else(|| {
            EngineError::new(
                code,
                format!("{display} response included an invalid embedding"),
            )
            .with_context(format!("model={reference} index={index}"))
        })?;
        vector.push(component as f32);
    }
    Ok(vector)
}

/// Standard base64 encoding (the `base64` crate is not a `zg-core`
/// dependency, so images are encoded by hand like `bytesToBase64`).
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let Some((&first, rest)) = chunk.split_first() else {
            continue;
        };
        let second = rest.first().copied().unwrap_or(0);
        let third = rest.get(1).copied().unwrap_or(0);
        let triple = (u32::from(first) << 16) | (u32::from(second) << 8) | u32::from(third);
        if let Some(&alphabet_char) = ALPHABET.get(((triple >> 18) & 0x3f) as usize) {
            output.push(alphabet_char as char);
        }
        if let Some(&alphabet_char) = ALPHABET.get(((triple >> 12) & 0x3f) as usize) {
            output.push(alphabet_char as char);
        }
        if chunk.len() > 1 {
            if let Some(&alphabet_char) = ALPHABET.get(((triple >> 6) & 0x3f) as usize) {
                output.push(alphabet_char as char);
            }
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            if let Some(&alphabet_char) = ALPHABET.get((triple & 0x3f) as usize) {
                output.push(alphabet_char as char);
            }
        } else {
            output.push('=');
        }
    }
    output
}

fn image_format_name(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Jpeg => "jpeg",
        ImageFormat::Png => "png",
        ImageFormat::Webp => "webp",
        ImageFormat::Gif => "gif",
    }
}

/// Qwen text embedding model (`text-embedding-v4`, `qwen3.7-text-embedding`).
///
/// Construct with [`QwenTextEmbeddingModel::from_plan`] using the resolved
/// `ModelBuildPlan::QwenText { entry, api_key, endpoint }` fields.
pub struct QwenTextEmbeddingModel {
    info: EmbeddingModelInfo,
    endpoint: String,
    api_key: ApiKey,
    agent: ureq::Agent,
    display_name: &'static str,
    model_kind: QwenTextModel,
    max_batch_size: usize,
    model: String,
}

impl QwenTextEmbeddingModel {
    /// Builds the backend from the resolved factory plan fields for the
    /// `ModelBuildPlan::QwenText` arm. Display name and error-code prefix
    /// follow the entry's model id, mirroring the two TypeScript subclasses.
    #[must_use]
    pub fn from_plan(entry: QwenTextEntry, api_key: ApiKey, endpoint: String) -> Self {
        let model_kind = QwenTextModel::from_model_id(entry.model);
        let display_name = model_kind.display_name();
        let info = EmbeddingModelInfo {
            reference: entry.reference.to_owned(),
            provider: entry.provider.to_owned(),
            model: entry.model.to_owned(),
            dimension: entry.dimension,
            metric: SearchMetric::Cosine,
            supports_images: false,
            max_input_tokens: Some(entry.max_input_tokens),
            input_kinds: vec![EmbeddingInputKind::Text],
            endpoint: Some(endpoint.clone()),
            default_concurrency: None,
        };
        Self {
            info,
            model: entry.model.to_owned(),
            endpoint,
            api_key,
            agent: remote_agent(),
            display_name,
            model_kind,
            max_batch_size: entry.max_batch_size,
        }
    }

    fn code(&self, failure: QwenTextFailure) -> EngineErrorCode {
        self.model_kind.code(failure)
    }

    fn embed_core(&self, inputs: &[EmbeddingInput<'_>]) -> EngineResult<EmbeddingResult> {
        let mut texts = Vec::with_capacity(inputs.len());
        for input in inputs {
            match input {
                EmbeddingInput::Text { text } => texts.push(*text),
                EmbeddingInput::Image { .. } => {
                    return Err(EngineError::from(ModelError::UnsupportedImage {
                        reference: self.info.reference.clone(),
                        index: None,
                    }));
                }
            }
        }
        let request = serde_json::json!({
            "model": self.model,
            "input": texts,
            "dimensions": self.info.dimension,
            "encoding_format": "float",
        });
        let body = post_embedding_json(
            &self.agent,
            self.display_name,
            &self.info.reference,
            &self.endpoint,
            &self.api_key,
            request,
            self.code(QwenTextFailure::RequestFailed),
            self.code(QwenTextFailure::InvalidJson),
            self.code(QwenTextFailure::ApiError),
        )?;
        let data = body.get("data").and_then(Value::as_array).ok_or_else(|| {
            EngineError::new(
                self.code(QwenTextFailure::MissingData),
                format!("{} response did not include data", self.display_name),
            )
            .with_context(format!("model={}", self.info.reference))
        })?;
        let mut vectors: Vec<Option<Vec<f32>>> = Vec::with_capacity(texts.len());
        vectors.resize_with(texts.len(), || None);
        for item in data {
            let object = item.as_object().ok_or_else(|| {
                EngineError::new(
                    self.code(QwenTextFailure::InvalidIndex),
                    format!("{} response included an invalid index", self.display_name),
                )
                .with_context(format!("model={} index=unknown", self.info.reference))
            })?;
            // Non-integer indices are invalid; integers (including negatives)
            // fall through to the range check, mirroring the TypeScript
            // `INVALID_INDEX` / `INDEX_OUT_OF_RANGE` split.
            let raw_index = object.get("index");
            let index = raw_index.and_then(Value::as_i64).ok_or_else(|| {
                let raw = raw_index
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".to_owned());
                EngineError::new(
                    self.code(QwenTextFailure::InvalidIndex),
                    format!("{} response included an invalid index", self.display_name),
                )
                .with_context(format!("model={} index={raw}", self.info.reference))
            })?;
            if index < 0 || index as u64 >= texts.len() as u64 {
                return Err(EngineError::new(
                    self.code(QwenTextFailure::IndexOutOfRange),
                    format!("{} response index was out of range", self.display_name),
                )
                .with_context(format!(
                    "model={} index={index} inputCount={}",
                    self.info.reference,
                    texts.len()
                )));
            }
            let index = index as usize;
            let vector = read_vector(
                self.code(QwenTextFailure::InvalidVector),
                self.display_name,
                &self.info.reference,
                index,
                item,
            )?;
            if let Some(slot) = vectors.get_mut(index) {
                *slot = Some(vector);
            }
        }
        let mut resolved = Vec::with_capacity(texts.len());
        for (vector_index, slot) in vectors.into_iter().enumerate() {
            match slot {
                Some(vector) => resolved.push(vector),
                None => {
                    return Err(EngineError::new(
                        self.code(QwenTextFailure::InvalidVector),
                        format!(
                            "{} response included an invalid embedding",
                            self.display_name
                        ),
                    )
                    .with_context(format!(
                        "model={} index={vector_index}",
                        self.info.reference
                    )));
                }
            }
        }
        Ok(EmbeddingResult {
            vectors: resolved,
            truncated: Vec::new(),
        })
    }
}

impl EmbeddingModel for QwenTextEmbeddingModel {
    fn info(&self) -> &EmbeddingModelInfo {
        &self.info
    }

    fn max_batch_size(&self) -> usize {
        self.max_batch_size
    }

    fn embed(
        &self,
        purpose: EmbeddingPurpose,
        inputs: &[EmbeddingInput<'_>],
    ) -> EngineResult<EmbeddingResult> {
        check_remote_embedding_permit(&self.info, purpose, inputs)?;
        embed_validated(self, inputs, || self.embed_core(inputs))
    }
}

/// Qwen multimodal embedding model (`qwen3-vl-embedding`): text plus images.
/// Construct with [`Qwen3VlEmbeddingModel::from_plan`] using the resolved
/// `ModelBuildPlan::QwenMultimodal { entry, api_key, endpoint }` fields.
pub struct Qwen3VlEmbeddingModel {
    info: EmbeddingModelInfo,
    model: String,
    endpoint: String,
    api_key: ApiKey,
    agent: ureq::Agent,
    max_batch_size: usize,
    max_image_bytes: u64,
}

impl Qwen3VlEmbeddingModel {
    /// Builds the backend from the resolved factory plan fields for the
    /// `ModelBuildPlan::QwenMultimodal` arm.
    #[must_use]
    pub fn from_plan(entry: QwenMultimodalEntry, api_key: ApiKey, endpoint: String) -> Self {
        let info = EmbeddingModelInfo {
            reference: entry.reference.to_owned(),
            provider: entry.provider.to_owned(),
            model: entry.model.to_owned(),
            dimension: entry.dimension,
            metric: SearchMetric::Cosine,
            supports_images: true,
            max_input_tokens: Some(entry.max_input_tokens),
            input_kinds: vec![EmbeddingInputKind::Text, EmbeddingInputKind::Image],
            endpoint: Some(endpoint.clone()),
            default_concurrency: None,
        };
        let agent = remote_agent();
        Self {
            info,
            model: entry.model.to_owned(),
            endpoint,
            api_key,
            agent,
            max_batch_size: entry.max_batch_size,
            max_image_bytes: entry.max_image_bytes,
        }
    }

    fn code(failure: QwenVlFailure) -> EngineErrorCode {
        qwen_vl_code(failure)
    }

    /// Enforces the VL-only input rules: supported image formats, the
    /// per-batch image count, and the per-image byte limit, mirroring
    /// `validateQwen3VlContents` plus the base-class image size check.
    fn validate_vl_inputs(&self, inputs: &[EmbeddingInput<'_>]) -> EngineResult<()> {
        let mut image_count = 0usize;
        for (index, input) in inputs.iter().enumerate() {
            if let EmbeddingInput::Image { data, format } = input {
                image_count = image_count.saturating_add(1);
                match format {
                    ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::Webp => {}
                    ImageFormat::Gif => {
                        return Err(EngineError::new(
                            Self::code(QwenVlFailure::UnsupportedImageFormat),
                            "Qwen3 VL embedding model does not support image format",
                        )
                        .with_context(format!(
                            "model={} index={index} format={}",
                            self.info.reference,
                            image_format_name(*format)
                        )));
                    }
                }
                if data.len() as u64 > self.max_image_bytes {
                    return Err(EngineError::from(ModelError::ImageTooLarge {
                        reference: self.info.reference.clone(),
                        index,
                        bytes: data.len() as u64,
                        max_bytes: self.max_image_bytes,
                    }));
                }
            }
        }
        if image_count > VL_MAX_IMAGE_COUNT {
            return Err(EngineError::new(
                Self::code(QwenVlFailure::TooManyImages),
                "Qwen3 VL embedding image count exceeds model limit",
            )
            .with_context(format!(
                "model={} imageCount={image_count} maxImageCount={VL_MAX_IMAGE_COUNT}",
                self.info.reference
            )));
        }
        Ok(())
    }

    fn embed_core(&self, inputs: &[EmbeddingInput<'_>]) -> EngineResult<EmbeddingResult> {
        self.validate_vl_inputs(inputs)?;
        let mut contents = Vec::with_capacity(inputs.len());
        for input in inputs {
            match input {
                EmbeddingInput::Text { text } => {
                    contents.push(serde_json::json!({ "text": text }));
                }
                EmbeddingInput::Image { data, .. } => {
                    contents.push(serde_json::json!({ "image": base64_encode(data) }));
                }
            }
        }
        let request = serde_json::json!({
            "model": self.model,
            "input": { "contents": contents },
            "parameters": { "dimension": self.info.dimension },
        });
        let body = post_embedding_json(
            &self.agent,
            "Qwen3 VL embedding",
            &self.info.reference,
            &self.endpoint,
            &self.api_key,
            request,
            Self::code(QwenVlFailure::RequestFailed),
            Self::code(QwenVlFailure::InvalidJson),
            Self::code(QwenVlFailure::ApiError),
        )?;
        let embeddings = body
            .get("output")
            .and_then(|output| output.get("embeddings"))
            .and_then(Value::as_array)
            .ok_or_else(|| {
                EngineError::new(
                    Self::code(QwenVlFailure::MissingEmbeddings),
                    "Qwen3 VL embedding response did not include embeddings",
                )
                .with_context(format!("model={}", self.info.reference))
            })?;
        let mut vectors: Vec<Option<Vec<f32>>> = Vec::with_capacity(inputs.len());
        vectors.resize_with(inputs.len(), || None);
        for (fallback_index, item) in embeddings.iter().enumerate() {
            let object = item.as_object().ok_or_else(|| {
                EngineError::new(
                    Self::code(QwenVlFailure::InvalidItem),
                    "Qwen3 VL embedding response included an invalid embedding item",
                )
                .with_context(format!(
                    "model={} index={fallback_index}",
                    self.info.reference
                ))
            })?;
            let index = read_embedding_index(object, fallback_index);
            if index < 0 || index as u64 >= inputs.len() as u64 {
                return Err(EngineError::new(
                    Self::code(QwenVlFailure::IndexOutOfRange),
                    "Qwen3 VL embedding response index was out of range",
                )
                .with_context(format!(
                    "model={} index={index} inputCount={}",
                    self.info.reference,
                    inputs.len()
                )));
            }
            let index = index as usize;
            let vector = read_vector(
                Self::code(QwenVlFailure::InvalidVector),
                "Qwen3 VL embedding",
                &self.info.reference,
                index,
                item,
            )?;
            if let Some(slot) = vectors.get_mut(index) {
                *slot = Some(vector);
            }
        }
        let mut resolved = Vec::with_capacity(inputs.len());
        for (vector_index, slot) in vectors.into_iter().enumerate() {
            match slot {
                Some(vector) => resolved.push(vector),
                None => {
                    return Err(EngineError::new(
                        Self::code(QwenVlFailure::InvalidVector),
                        "Qwen3 VL embedding response included an invalid embedding",
                    )
                    .with_context(format!(
                        "model={} index={vector_index}",
                        self.info.reference
                    )));
                }
            }
        }
        Ok(EmbeddingResult {
            vectors: resolved,
            truncated: Vec::new(),
        })
    }
}

/// Fails closed without an ambient operation permit, before any validation
/// or network traffic. Runs first so revoked or missing grants surface as
/// `AUTH.REMOTE_EMBEDDING_REQUIRED` even for otherwise-invalid inputs.
fn check_remote_embedding_permit(
    info: &EmbeddingModelInfo,
    purpose: EmbeddingPurpose,
    inputs: &[EmbeddingInput<'_>],
) -> EngineResult<()> {
    let mut content_kinds = Vec::with_capacity(2);
    if inputs
        .iter()
        .any(|input| matches!(input, EmbeddingInput::Text { .. }))
    {
        content_kinds.push(ContentKind::Text);
    }
    if inputs
        .iter()
        .any(|input| matches!(input, EmbeddingInput::Image { .. }))
    {
        content_kinds.push(ContentKind::Image);
    }
    RemoteEmbeddingGuard::new().check(&RemoteEmbeddingRequest {
        provider: info.provider.clone(),
        model: info.model.clone(),
        endpoint: info.endpoint.clone().unwrap_or_default(),
        purpose: match purpose {
            EmbeddingPurpose::Query => RemoteEmbeddingPurpose::Query,
            EmbeddingPurpose::Document => RemoteEmbeddingPurpose::Document,
        },
        content_kinds,
        content_count: inputs.len(),
    })
}

/// Reads the VL embedding index: `index`, else `text_index`, else the
/// item's position, mirroring `readEmbeddingIndex`. Integers (including
/// negatives) are returned as-is so the caller range-checks them.
fn read_embedding_index(item: &serde_json::Map<String, Value>, fallback: usize) -> i64 {
    for key in ["index", "text_index"] {
        if let Some(index) = item.get(key).and_then(Value::as_i64) {
            return index;
        }
    }
    fallback as i64
}

impl EmbeddingModel for Qwen3VlEmbeddingModel {
    fn info(&self) -> &EmbeddingModelInfo {
        &self.info
    }

    fn max_batch_size(&self) -> usize {
        self.max_batch_size
    }

    fn embed(
        &self,
        purpose: EmbeddingPurpose,
        inputs: &[EmbeddingInput<'_>],
    ) -> EngineResult<EmbeddingResult> {
        check_remote_embedding_permit(&self.info, purpose, inputs)?;
        embed_validated(self, inputs, || self.embed_core(inputs))
    }
}

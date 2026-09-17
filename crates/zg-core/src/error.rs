//! Engine error taxonomy mirroring `ZVEC_GREP.ENGINE.*` codes from the
//! TypeScript implementation, with context lines and secret redaction.

use std::borrow::Cow;
use std::fmt;

/// Prefix shared by every engine error code.
pub const ENGINE_ERROR_CODE_PREFIX: &str = "ZVEC_GREP.ENGINE";

/// Declares [`EngineErrorCode`] and its wire table in one place.
///
/// One macro arm per code generates the enum variant, its
/// [`EngineErrorCode::suffix`] literal, and its [`EngineErrorCode::all_codes`]
/// registration together, so the three can never drift: adding a code is one
/// arm, and every use site keeps compiling by construction.
macro_rules! define_engine_error_codes {
    ($($variant:ident => $suffix:literal),* $(,)?) => {
        /// Fully-qualified engine error code, e.g. `ZVEC_GREP.ENGINE.CONFIG.INVALID`.
        ///
        /// A closed `#[non_exhaustive]` enum: codes are genuinely matchable, there is
        /// exactly one literal per variant (in [`EngineErrorCode::suffix`]), and no public
        /// constructor takes a runtime string — assembling a code from one is impossible
        /// by construction. The wire string is the contract and never changes.
        /// `Copy`, allocation-free.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[non_exhaustive]
        pub enum EngineErrorCode {
            $($variant,)*
        }

        impl EngineErrorCode {
            /// The dotted suffix without the `ZVEC_GREP.ENGINE.` prefix.
            ///
            /// `const` so domain error enums can map variants to codes in `const fn`.
            #[must_use]
            pub const fn suffix(self) -> &'static str {
                match self {
                    $(Self::$variant => $suffix,)*
                }
            }

            /// Every engine wire code, one per variant, for the golden registry test
            /// (`tests/golden/error-codes.txt`). Emitted from the same arms as the
            /// enum, so a new variant is registered here by construction.
            #[must_use]
            pub fn all_codes() -> Vec<Self> {
                vec![$(Self::$variant,)*]
            }
        }
    };
}

define_engine_error_codes! {
    AuthInvalidTarget => "AUTH.INVALID_TARGET",
    AuthRemoteEmbeddingRequired => "AUTH.REMOTE_EMBEDDING_REQUIRED",
    AuthStoreFailed => "AUTH.STORE_FAILED",
    CliAuthorizationDeclined => "CLI.AUTHORIZATION_DECLINED",
    CliAuthorizationRequired => "CLI.AUTHORIZATION_REQUIRED",
    CliConfigInvalid => "CLI.CONFIG_INVALID",
    CliDaemonUnavailable => "CLI.DAEMON_UNAVAILABLE",
    CliInstallRefused => "CLI.INSTALL_REFUSED",
    CliIoFailed => "CLI.IO_FAILED",
    CliNotReady => "CLI.NOT_READY",
    CliRgIncompatible => "CLI.RG_INCOMPATIBLE",
    CliServerIncompatible => "CLI.SERVER_INCOMPATIBLE",
    CliUsage => "CLI.USAGE",
    ConfigEmbeddingEnvironmentInvalid => "CONFIG.EMBEDDING_ENVIRONMENT_INVALID",
    ConfigInvalid => "CONFIG.INVALID",
    ConfigInvalidEmbeddingRuntime => "CONFIG.INVALID_EMBEDDING_RUNTIME",
    ContextEmptyQuery => "CONTEXT.EMPTY_QUERY",
    ContextWorkspaceIndexDisabled => "CONTEXT.WORKSPACE_INDEX_DISABLED",
    ContextWorkspaceIndexNotFound => "CONTEXT.WORKSPACE_INDEX_NOT_FOUND",
    DaemonBlockingJoinFailed => "DAEMON.BLOCKING_JOIN_FAILED",
    DaemonLeaseActive => "DAEMON_LEASE_ACTIVE",
    ExtractorsCodeInvalidChunkOverlap => "EXTRACTORS.CODE_INVALID_CHUNK_OVERLAP",
    ExtractorsCodeInvalidChunkSize => "EXTRACTORS.CODE_INVALID_CHUNK_SIZE",
    ExtractorsEmptyAbsolutePath => "EXTRACTORS.EMPTY_ABSOLUTE_PATH",
    ExtractorsEmptyFileId => "EXTRACTORS.EMPTY_FILE_ID",
    ExtractorsEmptyRelativePath => "EXTRACTORS.EMPTY_RELATIVE_PATH",
    ExtractorsImageEmptyData => "EXTRACTORS.IMAGE_EMPTY_DATA",
    ExtractorsMarkdownInvalidChunkOverlap => "EXTRACTORS.MARKDOWN_INVALID_CHUNK_OVERLAP",
    ExtractorsMarkdownInvalidChunkSize => "EXTRACTORS.MARKDOWN_INVALID_CHUNK_SIZE",
    ExtractorsTextInvalidChunkOverlap => "EXTRACTORS.TEXT_INVALID_CHUNK_OVERLAP",
    ExtractorsTextInvalidChunkSize => "EXTRACTORS.TEXT_INVALID_CHUNK_SIZE",
    FileSelectionTypesUnavailable => "FILE_SELECTION.TYPES_UNAVAILABLE",
    FileSelectionUnknownFileType => "FILE_SELECTION.UNKNOWN_FILE_TYPE",
    IndexingCancelled => "INDEXING.CANCELLED",
    IndexingContentHashFailed => "INDEXING.CONTENT_HASH_FAILED",
    IndexingDeleteFileFailed => "INDEXING.DELETE_FILE_FAILED",
    IndexingEmbeddingFragmentFailed => "INDEXING.EMBEDDING_FRAGMENT_FAILED",
    IndexingEmbeddingThreadFailed => "INDEXING.EMBEDDING_THREAD_FAILED",
    IndexingFilesFailed => "INDEXING.FILES_FAILED",
    IndexingOptimizeFailed => "INDEXING.OPTIMIZE_FAILED",
    IndexingReadSourceFailed => "INDEXING.READ_SOURCE_FAILED",
    IndexingSchedulerFailed => "INDEXING.SCHEDULER_FAILED",
    IndexingStatusFailed => "INDEXING.STATUS_FAILED",
    IndexingWorkspaceFailed => "INDEXING.WORKSPACE_FAILED",
    JsonReadFailed => "JSON.READ_FAILED",
    JsonWriteFailed => "JSON.WRITE_FAILED",
    LexicalEmptyPattern => "LEXICAL.EMPTY_PATTERN",
    LexicalIgnoreFileInvalid => "LEXICAL.IGNORE_FILE_INVALID",
    LexicalInvalidPattern => "LEXICAL.INVALID_PATTERN",
    LexicalPatternFileUnreadable => "LEXICAL.PATTERN_FILE_UNREADABLE",
    LexicalSearchFailed => "LEXICAL.SEARCH_FAILED",
    LexicalUnknownFileType => "LEXICAL.UNKNOWN_FILE_TYPE",
    LockBusy => "LOCK.BUSY",
    LockUnavailable => "LOCK.UNAVAILABLE",
    ManifestDeleteFailed => "MANIFEST.DELETE_FAILED",
    ManifestInvalid => "MANIFEST.INVALID",
    ModelsEmbeddingBackendUnavailable => "MODELS.EMBEDDING_BACKEND_UNAVAILABLE",
    ModelsEmbeddingBatchTooLarge => "MODELS.EMBEDDING_BATCH_TOO_LARGE",
    ModelsEmbeddingCatalogModelNotFound => "MODELS.EMBEDDING_CATALOG_MODEL_NOT_FOUND",
    ModelsEmbeddingDimensionMismatch => "MODELS.EMBEDDING_DIMENSION_MISMATCH",
    ModelsEmbeddingEmptyImage => "MODELS.EMBEDDING_EMPTY_IMAGE",
    ModelsEmbeddingEmptyInput => "MODELS.EMBEDDING_EMPTY_INPUT",
    ModelsEmbeddingEmptyText => "MODELS.EMBEDDING_EMPTY_TEXT",
    ModelsEmbeddingImageTooLarge => "MODELS.EMBEDDING_IMAGE_TOO_LARGE",
    ModelsEmbeddingInvalidTruncatedInputIndex => "MODELS.EMBEDDING_INVALID_TRUNCATED_INPUT_INDEX",
    ModelsEmbeddingModelNotImplemented => "MODELS.EMBEDDING_MODEL_NOT_IMPLEMENTED",
    ModelsEmbeddingNonFiniteVectorValue => "MODELS.EMBEDDING_NON_FINITE_VECTOR_VALUE",
    ModelsEmbeddingUnsupportedContent => "MODELS.EMBEDDING_UNSUPPORTED_CONTENT",
    ModelsEmbeddingVectorCountMismatch => "MODELS.EMBEDDING_VECTOR_COUNT_MISMATCH",
    ModelsLlamaCppDisposed => "MODELS.LLAMA_CPP_DISPOSED",
    ModelsLlamaCppEmbedFailed => "MODELS.LLAMA_CPP_EMBED_FAILED",
    ModelsLlamaCppInvalidGguf => "MODELS.LLAMA_CPP_INVALID_GGUF",
    ModelsLlamaCppInvalidGgufHtml => "MODELS.LLAMA_CPP_INVALID_GGUF_HTML",
    ModelsModel2vecDownloadFailed => "MODELS.MODEL2VEC_DOWNLOAD_FAILED",
    ModelsModel2vecEmbedFailed => "MODELS.MODEL2VEC_EMBED_FAILED",
    ModelsModel2vecLoadFailed => "MODELS.MODEL2VEC_LOAD_FAILED",
    ModelsModelDownloadFailed => "MODELS.MODEL_DOWNLOAD_FAILED",
    ModelsQwen37TextEmbeddingApiError => "MODELS.QWEN37_TEXT_EMBEDDING_API_ERROR",
    ModelsQwen37TextEmbeddingIndexOutOfRange => "MODELS.QWEN37_TEXT_EMBEDDING_INDEX_OUT_OF_RANGE",
    ModelsQwen37TextEmbeddingInvalidIndex => "MODELS.QWEN37_TEXT_EMBEDDING_INVALID_INDEX",
    ModelsQwen37TextEmbeddingInvalidJson => "MODELS.QWEN37_TEXT_EMBEDDING_INVALID_JSON",
    ModelsQwen37TextEmbeddingInvalidVector => "MODELS.QWEN37_TEXT_EMBEDDING_INVALID_VECTOR",
    ModelsQwen37TextEmbeddingMissingApiKey => "MODELS.QWEN37_TEXT_EMBEDDING_MISSING_API_KEY",
    ModelsQwen37TextEmbeddingMissingData => "MODELS.QWEN37_TEXT_EMBEDDING_MISSING_DATA",
    ModelsQwen37TextEmbeddingMissingEndpoint => "MODELS.QWEN37_TEXT_EMBEDDING_MISSING_ENDPOINT",
    ModelsQwen37TextEmbeddingRequestFailed => "MODELS.QWEN37_TEXT_EMBEDDING_REQUEST_FAILED",
    ModelsQwen3VlEmbeddingApiError => "MODELS.QWEN3_VL_EMBEDDING_API_ERROR",
    ModelsQwen3VlEmbeddingIndexOutOfRange => "MODELS.QWEN3_VL_EMBEDDING_INDEX_OUT_OF_RANGE",
    ModelsQwen3VlEmbeddingInvalidItem => "MODELS.QWEN3_VL_EMBEDDING_INVALID_ITEM",
    ModelsQwen3VlEmbeddingInvalidJson => "MODELS.QWEN3_VL_EMBEDDING_INVALID_JSON",
    ModelsQwen3VlEmbeddingInvalidVector => "MODELS.QWEN3_VL_EMBEDDING_INVALID_VECTOR",
    ModelsQwen3VlEmbeddingMissingApiKey => "MODELS.QWEN3_VL_EMBEDDING_MISSING_API_KEY",
    ModelsQwen3VlEmbeddingMissingEmbeddings => "MODELS.QWEN3_VL_EMBEDDING_MISSING_EMBEDDINGS",
    ModelsQwen3VlEmbeddingMissingEndpoint => "MODELS.QWEN3_VL_EMBEDDING_MISSING_ENDPOINT",
    ModelsQwen3VlEmbeddingRequestFailed => "MODELS.QWEN3_VL_EMBEDDING_REQUEST_FAILED",
    ModelsQwen3VlEmbeddingTooManyImages => "MODELS.QWEN3_VL_EMBEDDING_TOO_MANY_IMAGES",
    ModelsQwen3VlEmbeddingUnsupportedImageFormat => "MODELS.QWEN3_VL_EMBEDDING_UNSUPPORTED_IMAGE_FORMAT",
    ModelsQwenTextEmbeddingApiError => "MODELS.QWEN_TEXT_EMBEDDING_API_ERROR",
    ModelsQwenTextEmbeddingIndexOutOfRange => "MODELS.QWEN_TEXT_EMBEDDING_INDEX_OUT_OF_RANGE",
    ModelsQwenTextEmbeddingInvalidIndex => "MODELS.QWEN_TEXT_EMBEDDING_INVALID_INDEX",
    ModelsQwenTextEmbeddingInvalidJson => "MODELS.QWEN_TEXT_EMBEDDING_INVALID_JSON",
    ModelsQwenTextEmbeddingInvalidVector => "MODELS.QWEN_TEXT_EMBEDDING_INVALID_VECTOR",
    ModelsQwenTextEmbeddingMissingApiKey => "MODELS.QWEN_TEXT_EMBEDDING_MISSING_API_KEY",
    ModelsQwenTextEmbeddingMissingData => "MODELS.QWEN_TEXT_EMBEDDING_MISSING_DATA",
    ModelsQwenTextEmbeddingMissingEndpoint => "MODELS.QWEN_TEXT_EMBEDDING_MISSING_ENDPOINT",
    ModelsQwenTextEmbeddingRequestFailed => "MODELS.QWEN_TEXT_EMBEDDING_REQUEST_FAILED",
    ModelsQwenTextEmbeddingV4ApiError => "MODELS.QWEN_TEXT_EMBEDDING_V4_API_ERROR",
    ModelsQwenTextEmbeddingV4IndexOutOfRange => "MODELS.QWEN_TEXT_EMBEDDING_V4_INDEX_OUT_OF_RANGE",
    ModelsQwenTextEmbeddingV4InvalidIndex => "MODELS.QWEN_TEXT_EMBEDDING_V4_INVALID_INDEX",
    ModelsQwenTextEmbeddingV4InvalidJson => "MODELS.QWEN_TEXT_EMBEDDING_V4_INVALID_JSON",
    ModelsQwenTextEmbeddingV4InvalidVector => "MODELS.QWEN_TEXT_EMBEDDING_V4_INVALID_VECTOR",
    ModelsQwenTextEmbeddingV4MissingApiKey => "MODELS.QWEN_TEXT_EMBEDDING_V4_MISSING_API_KEY",
    ModelsQwenTextEmbeddingV4MissingData => "MODELS.QWEN_TEXT_EMBEDDING_V4_MISSING_DATA",
    ModelsQwenTextEmbeddingV4MissingEndpoint => "MODELS.QWEN_TEXT_EMBEDDING_V4_MISSING_ENDPOINT",
    ModelsQwenTextEmbeddingV4RequestFailed => "MODELS.QWEN_TEXT_EMBEDDING_V4_REQUEST_FAILED",
    ModelsTransformersJsDisposed => "MODELS.TRANSFORMERS_JS_DISPOSED",
    ModelsTransformersJsEmbedFailed => "MODELS.TRANSFORMERS_JS_EMBED_FAILED",
    ModelsTransformersJsInvalidTensor => "MODELS.TRANSFORMERS_JS_INVALID_TENSOR",
    ModelsTransformersJsTokenizationFailed => "MODELS.TRANSFORMERS_JS_TOKENIZATION_FAILED",
    ScannerConfiguredIgnoreReadFailed => "SCANNER.CONFIGURED_IGNORE_READ_FAILED",
    ScannerOverlappingRootPaths => "SCANNER.OVERLAPPING_ROOT_PATHS",
    ScannerRootPathStatFailed => "SCANNER.ROOT_PATH_STAT_FAILED",
    ScannerUnsupportedRootPath => "SCANNER.UNSUPPORTED_ROOT_PATH",
    SearchDiagnosisEncodeFailed => "SEARCH.DIAGNOSIS_ENCODE_FAILED",
    SearchEmbeddingModelRequired => "SEARCH.EMBEDDING_MODEL_REQUIRED",
    SearchEntityNotFound => "SEARCH.ENTITY_NOT_FOUND",
    SearchPlanEmptyRoutes => "SEARCH_PLAN.EMPTY_ROUTES",
    SearchPlanEmptyRouteQuery => "SEARCH_PLAN.EMPTY_ROUTE_QUERY",
    SearchPlanInvalidFilter => "SEARCH_PLAN.INVALID_FILTER",
    SearchPlanInvalidModifiedTimeFilter => "SEARCH_PLAN.INVALID_MODIFIED_TIME_FILTER",
    SearchPlanInvalidModifiedTimeRange => "SEARCH_PLAN.INVALID_MODIFIED_TIME_RANGE",
    SearchPlanInvalidPathFilter => "SEARCH_PLAN.INVALID_PATH_FILTER",
    ServiceEmptyRouteQuery => "SERVICE.EMPTY_ROUTE_QUERY",
    ServiceReadSessionClosed => "SERVICE.READ_SESSION_CLOSED",
    StorageCollectionClosed => "STORAGE.COLLECTION_CLOSED",
    StorageCreateFailed => "STORAGE.CREATE_FAILED",
    StorageDeleteFailed => "STORAGE.DELETE_FAILED",
    StorageDocDecodeFailed => "STORAGE.DOC_DECODE_FAILED",
    StorageDocEncodeFailed => "STORAGE.DOC_ENCODE_FAILED",
    StorageDocFieldFailed => "STORAGE.DOC_FIELD_FAILED",
    StorageDuplicateFragmentId => "STORAGE.DUPLICATE_FRAGMENT_ID",
    StorageEntityVectorCountMismatch => "STORAGE.ENTITY_VECTOR_COUNT_MISMATCH",
    StorageFileMetaCorrupt => "STORAGE.FILE_META_CORRUPT",
    StorageFileMetaReadOnly => "STORAGE.FILE_META_READ_ONLY",
    StorageForeignTsIndexPresent => "STORAGE.FOREIGN_TS_INDEX_PRESENT",
    StorageFragmentFileMismatch => "STORAGE.FRAGMENT_FILE_MISMATCH",
    StorageInvalidEmbeddingDimension => "STORAGE.INVALID_EMBEDDING_DIMENSION",
    StorageInvalidFragmentGroup => "STORAGE.INVALID_FRAGMENT_GROUP",
    StorageInvalidStoragePath => "STORAGE.INVALID_STORAGE_PATH",
    StorageMissingEmbeddingSchema => "STORAGE.MISSING_EMBEDDING_SCHEMA",
    StorageReadOnly => "STORAGE.READ_ONLY",
    StorageSchemaFailed => "STORAGE.SCHEMA_FAILED",
    StorageUnsupportedStoredContentKind => "STORAGE.UNSUPPORTED_STORED_CONTENT_KIND",
    StorageZvecCollectionMissing => "STORAGE.ZVEC_COLLECTION_MISSING",
    StorageZvecDeleteFailed => "STORAGE.ZVEC_DELETE_FAILED",
    StorageZvecFetchFailed => "STORAGE.ZVEC_FETCH_FAILED",
    StorageZvecFileMetaMissing => "STORAGE.ZVEC_FILE_META_MISSING",
    StorageZvecInitFailed => "STORAGE.ZVEC_INIT_FAILED",
    StorageZvecOpenFailed => "STORAGE.ZVEC_OPEN_FAILED",
    StorageZvecOptimizeFailed => "STORAGE.ZVEC_OPTIMIZE_FAILED",
    StorageZvecQueryFailed => "STORAGE.ZVEC_QUERY_FAILED",
    StorageZvecUpsertFailed => "STORAGE.ZVEC_UPSERT_FAILED",
    WorkspaceRootUnavailable => "WORKSPACE.ROOT_UNAVAILABLE",
    WorkspaceIndexEmbeddingDimensionMismatch => "WORKSPACE_INDEX.EMBEDDING_DIMENSION_MISMATCH",
    WorkspaceIndexEmbeddingMetricMismatch => "WORKSPACE_INDEX.EMBEDDING_METRIC_MISMATCH",
    WorkspaceIndexEmbeddingModelMismatch => "WORKSPACE_INDEX.EMBEDDING_MODEL_MISMATCH",
    WorkspaceIndexEmbeddingModelRequired => "WORKSPACE_INDEX.EMBEDDING_MODEL_REQUIRED",
    WorkspaceIndexEmbeddingProviderMismatch => "WORKSPACE_INDEX.EMBEDDING_PROVIDER_MISMATCH",
    WorkspaceIndexMissing => "WORKSPACE_INDEX.MISSING",
    WorkspaceIndexReadOnly => "WORKSPACE_INDEX.READ_ONLY",
    WorkspaceIndexVersionMismatch => "WORKSPACE_INDEX.VERSION_MISMATCH",
}

impl EngineErrorCode {
    /// The fully-qualified wire string, e.g. `ZVEC_GREP.ENGINE.CONFIG.INVALID`.
    #[must_use]
    pub fn qualified(self) -> String {
        format!("{ENGINE_ERROR_CODE_PREFIX}.{}", self.suffix())
    }
}

impl fmt::Display for EngineErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{ENGINE_ERROR_CODE_PREFIX}.{}", self.suffix())
    }
}

/// Serializes as the dotted suffix (e.g. `"CONFIG.INVALID"`), matching the
/// previous `&'static str` representation byte-for-byte.
impl serde::Serialize for EngineErrorCode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.suffix())
    }
}

/// Engine error with a dotted code, message, multi-line context detail, and
/// an optional typed cause.
///
/// The cause rides in an `Arc` (not a `Box`): `EngineError` is `Clone`
/// (fail-fast paths clone the first failure), and `Arc` keeps that.
///
/// `Display` and `Debug` redact secrets (see [`redact_error_text`]): every
/// `to_string()` / logging sink observes redacted text with no per-callsite
/// work. The [`EngineError::message`] / [`EngineError::context`] accessors
/// stay raw for programmatic use; format or log through `Display`/`Debug`.
#[derive(Clone)]
pub struct EngineError {
    code: EngineErrorCode,
    message: String,
    /// Optional multi-line `key=value` context block.
    context: Option<String>,
    source: Option<std::sync::Arc<dyn std::error::Error + Send + Sync>>,
}

impl EngineError {
    pub fn new(code: EngineErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            context: None,
            source: None,
        }
    }

    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
    }

    /// Attaches the typed cause (an `io::Error`, `serde_json::Error`, …)
    /// instead of flattening it into the context string. Prefer this at
    /// conversion sites; keep `with_context` for the `key=value` detail.
    pub fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(std::sync::Arc::new(source));
        self
    }

    #[must_use]
    pub fn code(&self) -> &EngineErrorCode {
        &self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub fn context(&self) -> Option<&str> {
        self.context.as_deref()
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Redaction at the Display boundary (issue #22): `usize::MAX`
        // disables truncation, so this pass only redacts. Idempotent with
        // per-callsite `redact_error_text` (already-redacted markers are
        // skipped by lookahead), so double redaction is a no-op.
        let message = redact_error_text(&self.message, usize::MAX);
        write!(f, "{}: {}", self.code, message)?;
        if let Some(context) = &self.context {
            let context = redact_error_text(context, usize::MAX);
            write!(f, "\n{context}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Same boundary as `Display`: `{:?}` logging must never leak secrets.
        let message = redact_error_text(&self.message, usize::MAX).into_owned();
        let context: Option<String> = self
            .context
            .as_deref()
            .map(|context| redact_error_text(context, usize::MAX).into_owned());
        f.debug_struct("EngineError")
            .field("code", &self.code)
            .field("message", &message)
            .field("context", &context)
            .field("source", &self.source)
            .finish()
    }
}

impl std::error::Error for EngineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|cause| cause as _)
    }
}

pub type EngineResult<T> = Result<T, EngineError>;

/// Well-known engine error codes.
///
/// Every constructor is `const` and takes no runtime input: codes are
/// literals, never assembled. The `extractor` escape hatch that once took a
/// runtime `suffix: &str` is gone; each extractor code is its own literal
/// below (M1). See `tests/golden/error-codes.txt` for the full registry.
pub mod codes {
    use super::EngineErrorCode;

    #[must_use]
    pub const fn config_invalid() -> EngineErrorCode {
        EngineErrorCode::ConfigInvalid
    }

    #[must_use]
    pub const fn config_invalid_embedding_runtime() -> EngineErrorCode {
        EngineErrorCode::ConfigInvalidEmbeddingRuntime
    }

    #[must_use]
    pub const fn manifest_invalid() -> EngineErrorCode {
        EngineErrorCode::ManifestInvalid
    }

    #[must_use]
    pub const fn lock_busy() -> EngineErrorCode {
        EngineErrorCode::LockBusy
    }

    #[must_use]
    pub const fn daemon_lease_active() -> EngineErrorCode {
        EngineErrorCode::DaemonLeaseActive
    }

    /// A `spawn_blocking` body panicked or was aborted (daemon join failure).
    #[must_use]
    pub const fn daemon_blocking_join_failed() -> EngineErrorCode {
        EngineErrorCode::DaemonBlockingJoinFailed
    }

    #[must_use]
    pub const fn service_empty_route_query() -> EngineErrorCode {
        EngineErrorCode::ServiceEmptyRouteQuery
    }

    #[must_use]
    pub const fn service_read_session_closed() -> EngineErrorCode {
        EngineErrorCode::ServiceReadSessionClosed
    }

    #[must_use]
    pub const fn extractor_code_invalid_chunk_size() -> EngineErrorCode {
        EngineErrorCode::ExtractorsCodeInvalidChunkSize
    }

    #[must_use]
    pub const fn extractor_code_invalid_chunk_overlap() -> EngineErrorCode {
        EngineErrorCode::ExtractorsCodeInvalidChunkOverlap
    }

    #[must_use]
    pub const fn extractor_markdown_invalid_chunk_size() -> EngineErrorCode {
        EngineErrorCode::ExtractorsMarkdownInvalidChunkSize
    }

    #[must_use]
    pub const fn extractor_markdown_invalid_chunk_overlap() -> EngineErrorCode {
        EngineErrorCode::ExtractorsMarkdownInvalidChunkOverlap
    }

    #[must_use]
    pub const fn extractor_text_invalid_chunk_size() -> EngineErrorCode {
        EngineErrorCode::ExtractorsTextInvalidChunkSize
    }

    #[must_use]
    pub const fn extractor_text_invalid_chunk_overlap() -> EngineErrorCode {
        EngineErrorCode::ExtractorsTextInvalidChunkOverlap
    }

    #[must_use]
    pub const fn extractor_empty_file_id() -> EngineErrorCode {
        EngineErrorCode::ExtractorsEmptyFileId
    }

    #[must_use]
    pub const fn extractor_empty_absolute_path() -> EngineErrorCode {
        EngineErrorCode::ExtractorsEmptyAbsolutePath
    }

    #[must_use]
    pub const fn extractor_empty_relative_path() -> EngineErrorCode {
        EngineErrorCode::ExtractorsEmptyRelativePath
    }

    #[must_use]
    pub const fn extractor_image_empty_data() -> EngineErrorCode {
        EngineErrorCode::ExtractorsImageEmptyData
    }
}

/// One entry for [`error_details`]: either a free-form line or a key/value pair.
pub enum DetailEntry<'a> {
    Line(&'a str),
    Pair(&'a str, DetailValue<'a>),
}

/// Scalar value allowed in error detail pairs.
#[derive(Debug, Clone, Copy)]
pub enum DetailValue<'a> {
    Str(&'a str),
    Int(i64),
    Uint(u64),
    Float(f64),
    Bool(bool),
    Null,
}

impl<'a> DetailValue<'a> {
    fn render(&self) -> String {
        match self {
            Self::Str(s) => (*s).to_owned(),
            Self::Int(v) => v.to_string(),
            Self::Uint(v) => v.to_string(),
            Self::Float(v) => v.to_string(),
            Self::Bool(v) => v.to_string(),
            Self::Null => "null".to_owned(),
        }
    }
}

/// Joins non-empty detail entries with newlines; `None` when nothing remains.
#[must_use]
pub fn error_details(entries: Vec<DetailEntry<'_>>) -> Option<String> {
    let mut lines = Vec::new();
    for entry in entries {
        match entry {
            DetailEntry::Line(text) => {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    lines.push(trimmed.to_owned());
                }
            }
            DetailEntry::Pair(key, value) => {
                if matches!(value, DetailValue::Null) {
                    continue;
                }
                lines.push(format!("{key}={}", value.render()));
            }
        }
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

/// `workspaceIndex=<name>` detail helper.
#[must_use]
pub fn workspace_index_detail(name: &str) -> String {
    format!("workspaceIndex={name}")
}

/// Redacts credentials from arbitrary error text, then truncates to `max_length`
/// with an ellipsis. Returns a borrow when nothing needs redacting or cutting,
/// so the common no-secret path allocates zero times.
///
/// Passes, in order (each borrows when it matches nothing):
/// 1. URL userinfo (`scheme://user@`, hand-rolled scanner: the Rust regex
///    crate has no lookbehind).
/// 2. Query params (`[?&]api_key=…`, `token=…`, …) — value runs to `&`.
/// 3. Header lines (`x-api-key: …`, `x-auth-token: …`, `authorization: …`,
///    except `bearer`/`basic` which the bearer pass owns).
/// 4. Quoted `key=value` / `key: value` (pre-existing).
/// 5. Unquoted `key=value` / `key: value` — value runs to whitespace or a
///    structural delimiter (`& " ' , ; ) > ]`).
/// 6. `Bearer` / `Basic` credentials (pre-existing).
/// 7. Known-prefix tokens: AWS `AKIA…`, GitHub `ghp_/gho_/ghu_/ghs_/ghr_…`,
///    Slack `xox…-…`, Google `AIza…`.
/// 8. `sk-…` tokens (pre-existing).
/// 9. PEM private-key blocks (whole `BEGIN…END` range), else the lone
///    `BEGIN … PRIVATE KEY` armor line.
///
/// New value passes skip already-redacted markers (checked in code, since the
/// `regex` crate has no look-around), so redacting twice — e.g. a per-callsite
/// pass plus the [`EngineError`] `Display` boundary — is a no-op. Benign
/// `key=value` detail pairs (`provider=…`, `model=…`, `count=3`) and plain URLs
/// match no pass and keep borrowing.
///
/// # Panics
///
/// Panics only when a `static` redaction regex fails to compile. The patterns
/// are literal constants that ship with the crate, so construction cannot fail
/// at runtime; Phase 2 converts these to `LazyLock<Result<Regex>>` with a
/// skip-pass fallback and removes this section.
// Phase 2 debt: `panic!` in `LazyLock` init must become typed error /
// skip-pass fallback (`LazyLock<Result<Regex>>`). Allowed here so the
// `panic = "deny"` firewall stays green until then.
#[allow(clippy::panic)]
#[must_use]
pub fn redact_error_text(value: &str, max_length: usize) -> Cow<'_, str> {
    use std::sync::LazyLock;

    use regex::Regex;

    static KEY_VALUE: LazyLock<Regex> = LazyLock::new(|| {
        // A credential-shaped key (optionally access/refresh/id-prefixed),
        // optionally quoted, followed by a quoted value.
        Regex::new(
            r#"(?i)(["']?(?:(?:access|refresh|id)[_ -]?)?(?:api[_ -]?key|token|authorization|password|secret)["']?\s*[:=]\s*)["'][^"']*["']"#,
        )
        .unwrap_or_else(|e| panic!("static redaction regex must compile: {e}"))
    });
    static QUERY_PARAM: LazyLock<Regex> = LazyLock::new(|| {
        // `?api_key=…` / `&token=…`: tight anchor (no gap allowed between
        // `?`/`&` and the key) so `monkey=` / `keyboard=` never match, and
        // the value stops at `&` so sibling params survive. The
        // already-redacted guard lives in code (see below): the `regex`
        // crate has no look-around.
        Regex::new(
            r#"(?i)([?&]["']?\b(?:(?:access|refresh|id)[_ -]?)?(?:api[_ -]?key|api[_ -]?secret|client[_ -]?secret|auth[_ -]?token|token|authorization|password|passwd|secret|key|auth)["']?\s*=)([^&\s"'#;]+)"#,
        )
        .unwrap_or_else(|e| panic!("static redaction regex must compile: {e}"))
    });
    static HEADER: LazyLock<Regex> = LazyLock::new(|| {
        // `x-api-key: …` / `x-auth-token: …` / `authorization: …`, value to
        // end of segment. `bearer`/`basic` are owned by the bearer pass
        // (keeps `Bearer [redacted]` stable); `["']` stops quoted values
        // for the quoted key/value pass. Both guards live in code (see
        // below): the `regex` crate has no look-around.
        Regex::new(r#"(?i)(\b(?:x-api-key|x-auth-token|authorization)\s*:\s*)([^\x0A\x0D\"'&,;]+)"#)
            .unwrap_or_else(|e| panic!("static redaction regex must compile: {e}"))
    });
    static KEY_VALUE_UNQUOTED: LazyLock<Regex> = LazyLock::new(|| {
        // Same key shapes without quotes: `api_key=s3cr3t`, `password: x`.
        // `\b` keeps `monkey=` / `keyboard=` intact; the value stops at
        // whitespace, quotes, or structural delimiters so trailing context
        // (`&next=1`, `)`, `]`) survives. `bearer`/`basic` values are owned
        // by the bearer pass (keeps `Authorization: Bearer [redacted]` stable).
        // Both guards live in code (see below): the `regex` crate has no
        // look-around.
        Regex::new(
            r#"(?i)(["']?\b(?:(?:access|refresh|id)[_ -]?)?(?:api[_ -]?key|api[_ -]?secret|client[_ -]?secret|auth[_ -]?token|token|authorization|password|passwd|secret|key|auth)["']?\s*[:=]\s*)([^\s"'&,;)>\]]+)"#,
        )
        .unwrap_or_else(|e| panic!("static redaction regex must compile: {e}"))
    });
    static BEARER: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(bearer|basic)\s+[a-z0-9._\-+/=]+")
            .unwrap_or_else(|e| panic!("static redaction regex must compile: {e}"))
    });
    static KNOWN_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
        // Provider-shaped tokens with no key context: AWS access key ids,
        // GitHub / Slack / Google API tokens.
        Regex::new(
            r"(?i)\b(?:AKIA[0-9A-Z]{16}|gh[opsr]_[A-Za-z0-9_]{8,}|xox[abdeoprs]-[0-9A-Za-z\-]{8,}|AIza[0-9A-Za-z\-_]{35})\b",
        )
        .unwrap_or_else(|e| panic!("static redaction regex must compile: {e}"))
    });
    static SK_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\bsk-[a-z0-9_\-]{8,}")
            .unwrap_or_else(|e| panic!("static redaction regex must compile: {e}"))
    });
    static PEM_BLOCK: LazyLock<Regex> = LazyLock::new(|| {
        // Whole armor-to-armor range so key material between the lines goes too.
        Regex::new(
            r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
        )
        .unwrap_or_else(|e| panic!("static redaction regex must compile: {e}"))
    });
    static PEM_HEADER: LazyLock<Regex> = LazyLock::new(|| {
        // Lone armor line with no closing `END` in scope.
        Regex::new(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----")
            .unwrap_or_else(|e| panic!("static redaction regex must compile: {e}"))
    });

    // Guards the `regex` crate cannot express as look-around: a value that
    // is already `[redacted]` passes through (double redaction is a no-op),
    // and a `bearer`/`basic`-led value stays owned by the bearer pass.
    // Bearer token chars mirror the `BEARER` class below (`[a-z0-9._\-+/=]`,
    // case-insensitive): `Bearer abc` skips, `bearertoken` still redacts.
    fn is_redacted_value(value: &str) -> bool {
        value.starts_with("[redacted")
    }
    fn is_bearer_value(value: &str) -> bool {
        let bytes = value.as_bytes();
        let prefix_len = if bytes
            .get(..6)
            .is_some_and(|p| p.eq_ignore_ascii_case(b"bearer"))
        {
            6
        } else if bytes
            .get(..5)
            .is_some_and(|p| p.eq_ignore_ascii_case(b"basic"))
        {
            5
        } else {
            return false;
        };
        match bytes.get(prefix_len) {
            None => true,
            Some(next) => !matches!(
                next,
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-' | b'+' | b'/' | b'='
            ),
        }
    }
    // Each pass borrows when it matches nothing, so the common no-secret
    // path allocates zero times; only an actual redaction forces ownership
    // (reassigned solely in the `Owned` arm, where nothing borrows `out`).
    let mut out = redact_url_userinfo(value);
    if let Cow::Owned(owned) = QUERY_PARAM.replace_all(&out, |caps: &regex::Captures| {
        if is_redacted_value(&caps[2]) {
            caps[0].to_owned()
        } else {
            format!("{}[redacted]", &caps[1])
        }
    }) {
        out = Cow::Owned(owned);
    }
    if let Cow::Owned(owned) = HEADER.replace_all(&out, |caps: &regex::Captures| {
        if is_redacted_value(&caps[2]) || is_bearer_value(&caps[2]) {
            caps[0].to_owned()
        } else {
            format!("{}[redacted]", &caps[1])
        }
    }) {
        out = Cow::Owned(owned);
    }
    if let Cow::Owned(owned) = KEY_VALUE.replace_all(&out, "$1\"[redacted]\"") {
        out = Cow::Owned(owned);
    }
    if let Cow::Owned(owned) = KEY_VALUE_UNQUOTED.replace_all(&out, |caps: &regex::Captures| {
        if is_redacted_value(&caps[2]) || is_bearer_value(&caps[2]) {
            caps[0].to_owned()
        } else {
            format!("{}[redacted]", &caps[1])
        }
    }) {
        out = Cow::Owned(owned);
    }
    for (pattern, replacement) in [
        (&BEARER, "$1 [redacted]"),
        (&KNOWN_TOKEN, "[redacted]"),
        (&SK_TOKEN, "sk-[redacted]"),
        (&PEM_BLOCK, "[redacted-private-key]"),
        (&PEM_HEADER, "-----BEGIN [redacted] PRIVATE KEY-----"),
    ] {
        if let Cow::Owned(owned) = pattern.replace_all(&out, replacement) {
            out = Cow::Owned(owned);
        }
    }
    truncate_cow(out, max_length)
}

/// Replaces `scheme://user@` userinfo with `[redacted]@`. The scheme must start
/// with an ASCII letter, not end in a punctuation char, and the byte before it
/// must not be a scheme character (mirrors the TS lookbehind).
fn redact_url_userinfo(value: &str) -> Cow<'_, str> {
    // Fast path: no scheme separator means no userinfo — borrow.
    if !value.contains("://") {
        return Cow::Borrowed(value);
    }
    let bytes = value.as_bytes();
    let mut result = String::with_capacity(value.len());
    let mut cursor = 0usize;
    let mut redacted = false;
    while let Some(offset) = value.get(cursor..).unwrap_or("").find("://") {
        let separator = cursor + offset;
        let mut start = separator;
        while start > 0 {
            let Some(b) = bytes.get(start - 1) else {
                break;
            };
            if b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-') {
                start -= 1;
            } else {
                break;
            }
        }
        let scheme_valid = start < separator
            && bytes.get(start).is_some_and(u8::is_ascii_alphabetic)
            && separator
                .checked_sub(1)
                .and_then(|index| bytes.get(index))
                .is_some_and(|b| !matches!(b, b'+' | b'.' | b'-'));

        let mut end = separator + 3;
        let mut terminator = None;
        while let Some(&b) = bytes.get(end) {
            match b {
                b'@' => {
                    terminator = Some(end);
                    break;
                }
                b'/' | b':' | b'?' | b'#' | b' ' | b'\t' | b'\n' | b'\r' => break,
                _ => end += 1,
            }
        }

        if scheme_valid && terminator.is_some() {
            result.push_str(value.get(cursor..start).unwrap_or(""));
            result.push_str("[redacted]@");
            cursor = end + 1;
            redacted = true;
        } else {
            let copy_through = (separator + 3).min(bytes.len());
            result.push_str(value.get(cursor..copy_through).unwrap_or(""));
            cursor = copy_through;
        }
    }
    if !redacted {
        return Cow::Borrowed(value);
    }
    result.push_str(value.get(cursor..).unwrap_or(""));
    Cow::Owned(result)
}

/// Truncates to `max_length` Unicode scalar values, keeping `max_length - 1`
/// leading chars plus `…`; never splits a char. Passes borrowing through
/// when nothing is cut.
fn truncate_cow(value: Cow<'_, str>, max_length: usize) -> Cow<'_, str> {
    if value.chars().count() <= max_length {
        return value;
    }
    let mut truncated: String = value.chars().take(max_length.saturating_sub(1)).collect();
    truncated.push('\u{2026}');
    Cow::Owned(truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_roundtrip() {
        let code = EngineErrorCode::ConfigInvalid;
        assert_eq!(code.suffix(), "CONFIG.INVALID");
        assert_eq!(code.qualified(), "ZVEC_GREP.ENGINE.CONFIG.INVALID");
        assert_eq!(code.to_string(), "ZVEC_GREP.ENGINE.CONFIG.INVALID");
        // `Copy`, not just `Clone`: codes move freely into error values.
        let copied = code;
        assert_eq!(copied, code);
    }

    #[test]
    fn redacts_bearer_and_api_keys() {
        let text = "GET /x Authorization: Bearer abc123.-_ and api_key=\"s3cr3t\" tail";
        let redacted = redact_error_text(text, 200);
        assert!(redacted.contains("Bearer [redacted]"));
        assert!(!redacted.contains("s3cr3t"));
    }

    #[test]
    fn clean_input_borrows_without_allocating() {
        assert!(matches!(
            redact_error_text("nothing secret here", 200),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn truncates_with_ellipsis() {
        let redacted = redact_error_text("abcdefghij", 5);
        assert_eq!(redacted.chars().count(), 5);
        assert!(redacted.ends_with('\u{2026}'));
    }

    #[test]
    fn source_chain_is_walkable() {
        let cause = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let error =
            EngineError::new(EngineErrorCode::JsonReadFailed, "failed to read").with_source(cause);
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn details_skip_empty_and_null() {
        let details = error_details(vec![
            DetailEntry::Line("  "),
            DetailEntry::Line("path=/tmp/x"),
            DetailEntry::Pair("count", DetailValue::Uint(3)),
            DetailEntry::Pair("missing", DetailValue::Null),
        ]);
        assert_eq!(details.as_deref(), Some("path=/tmp/x\ncount=3"));
    }
    #[test]
    fn redacts_unquoted_key_value() {
        let redacted = redact_error_text("request failed: api_key=s3cr3t123 tail", 200);
        assert!(redacted.contains("api_key=[redacted]"));
        assert!(!redacted.contains("s3cr3t123"));
        let redacted = redact_error_text("login failed password: hunter2 retry", 200);
        assert!(redacted.contains("password: [redacted]"));
        assert!(!redacted.contains("hunter2"));
        // Sibling params survive the value cut.
        let redacted = redact_error_text("token=abc&next=1", 200);
        assert_eq!(redacted.as_ref(), "token=[redacted]&next=1");
    }

    #[test]
    fn redacts_query_params() {
        let redacted = redact_error_text(
            "https://api.example.com/v1?q=test&api_key=s3cr3t&lang=en",
            200,
        );
        assert!(redacted.contains("api_key=[redacted]"));
        assert!(redacted.contains("q=test"));
        assert!(redacted.contains("lang=en"));
        assert!(!redacted.contains("s3cr3t"));
    }

    #[test]
    fn redacts_secret_headers() {
        let redacted = redact_error_text("x-api-key: s3cr3t-value", 200);
        assert_eq!(redacted.as_ref(), "x-api-key: [redacted]");
        let redacted = redact_error_text("X-Auth-Token: abc123", 200);
        assert_eq!(redacted.as_ref(), "X-Auth-Token: [redacted]");
        // `bearer` stays owned by the bearer pass.
        let redacted = redact_error_text("Authorization: Bearer abc123", 200);
        assert!(redacted.contains("Bearer [redacted]"));
        assert!(!redacted.contains("abc123"));
    }

    #[test]
    fn redacts_known_prefix_tokens() {
        let google = format!("key=AIza{}", "A".repeat(35));
        let text = "AKIAIOSFODNN7EXAMPLE ghp_1234567890abcdef1234567890abcdef1234 \
            xoxb-123456789012-1234567890123-AbCdEfGhIjKlMnOp sk-live-abcdefgh12345678 "
            .to_owned()
            + &google;
        let redacted = redact_error_text(&text, 400);
        assert!(!redacted.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(!redacted.contains("ghp_1234567890abcdef1234567890abcdef1234"));
        assert!(!redacted.contains("xoxb-123456789012-1234567890123-AbCdEfGhIjKlMnOp"));
        assert!(!redacted.contains(&google));
        assert!(redacted.contains("[redacted]"));
    }

    #[test]
    fn redacts_pem_private_key() {
        let nl = char::from(10);
        let pem = format!(
            "-----BEGIN RSA PRIVATE KEY-----{nl}MIIEowIBAAKCAQEA7bq3Z8{nl}-----END RSA PRIVATE KEY-----"
        );
        let redacted = redact_error_text(&pem, 400);
        assert!(!redacted.contains("MIIEowIBAAKCAQEA7bq3Z8"));
        assert!(redacted.contains("[redacted-private-key]"));
        // Lone armor line without a closing END.
        let redacted = redact_error_text("key -----BEGIN EC PRIVATE KEY----- tail", 200);
        assert!(redacted.contains("-----BEGIN [redacted] PRIVATE KEY-----"));
        // Non-key armor is not a secret.
        assert!(matches!(
            redact_error_text("-----BEGIN CERTIFICATE-----", 200),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn leaves_normal_urls_and_details_intact() {
        assert!(matches!(
            redact_error_text("https://example.com/search?q=rust&lang=en&page=2", 200),
            Cow::Borrowed(_)
        ));
        assert!(matches!(
            redact_error_text("provider=qwen model=text-embedding-v4 purpose=query", 200),
            Cow::Borrowed(_)
        ));
        assert!(matches!(
            redact_error_text("monkey=banana keyboard=1 count=3", 200),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn display_and_debug_redact_without_sink_changes() {
        let error = EngineError::new(EngineErrorCode::ConfigInvalid, "bad key api_key=s3cr3t123")
            .with_context("endpoint=https://example.invalid/e".to_owned());
        // Programmatic accessors stay raw …
        assert!(error.message().contains("s3cr3t123"));
        // … while the logging boundary redacts every sink at once.
        let shown = error.to_string();
        assert!(shown.contains("api_key=[redacted]"));
        assert!(!shown.contains("s3cr3t123"));
        assert!(shown.contains("endpoint=https://example.invalid/e"));
        let debugged = format!("{error:?}");
        assert!(!debugged.contains("s3cr3t123"));
        // Double redaction is a no-op (per-callsite pass + Display boundary).
        let twice = redact_error_text(&shown, usize::MAX);
        assert_eq!(twice.as_ref(), shown.as_str());
    }
}

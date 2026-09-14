//! Engine error taxonomy mirroring `ZVEC_GREP.ENGINE.*` codes from the
//! TypeScript implementation, with context lines and secret redaction.

use std::borrow::Cow;
use std::fmt;

/// Prefix shared by every engine error code.
pub const ENGINE_ERROR_CODE_PREFIX: &str = "ZVEC_GREP.ENGINE";

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
    AuthInvalidTarget,
    AuthRemoteEmbeddingRequired,
    AuthStoreFailed,
    CliAuthorizationDeclined,
    CliAuthorizationRequired,
    CliConfigInvalid,
    CliDaemonUnavailable,
    CliInstallRefused,
    CliIoFailed,
    CliNotReady,
    CliRgIncompatible,
    CliServerIncompatible,
    CliUsage,
    ConfigEmbeddingEnvironmentInvalid,
    ConfigInvalid,
    ConfigInvalidEmbeddingRuntime,
    ContextEmptyQuery,
    ContextWorkspaceIndexDisabled,
    ContextWorkspaceIndexNotFound,
    DaemonBlockingJoinFailed,
    DaemonLeaseActive,
    ExtractorsCodeInvalidChunkOverlap,
    ExtractorsCodeInvalidChunkSize,
    ExtractorsEmptyAbsolutePath,
    ExtractorsEmptyFileId,
    ExtractorsEmptyRelativePath,
    ExtractorsImageEmptyData,
    ExtractorsMarkdownInvalidChunkOverlap,
    ExtractorsMarkdownInvalidChunkSize,
    ExtractorsTextInvalidChunkOverlap,
    ExtractorsTextInvalidChunkSize,
    FileSelectionTypesUnavailable,
    FileSelectionUnknownFileType,
    IndexingCancelled,
    IndexingContentHashFailed,
    IndexingDeleteFileFailed,
    IndexingEmbeddingFragmentFailed,
    IndexingEmbeddingThreadFailed,
    IndexingFilesFailed,
    IndexingOptimizeFailed,
    IndexingReadSourceFailed,
    IndexingSchedulerFailed,
    IndexingStatusFailed,
    IndexingWorkspaceFailed,
    JsonReadFailed,
    JsonWriteFailed,
    LexicalEmptyPattern,
    LexicalIgnoreFileInvalid,
    LexicalInvalidPattern,
    LexicalPatternFileUnreadable,
    LexicalSearchFailed,
    LexicalUnknownFileType,
    LockBusy,
    LockUnavailable,
    ManifestDeleteFailed,
    ManifestInvalid,
    ModelsEmbeddingBackendUnavailable,
    ModelsEmbeddingBatchTooLarge,
    ModelsEmbeddingCatalogModelNotFound,
    ModelsEmbeddingDimensionMismatch,
    ModelsEmbeddingEmptyImage,
    ModelsEmbeddingEmptyInput,
    ModelsEmbeddingEmptyText,
    ModelsEmbeddingImageTooLarge,
    ModelsEmbeddingInvalidTruncatedInputIndex,
    ModelsEmbeddingModelNotImplemented,
    ModelsEmbeddingNonFiniteVectorValue,
    ModelsEmbeddingUnsupportedContent,
    ModelsEmbeddingVectorCountMismatch,
    ModelsLlamaCppDisposed,
    ModelsLlamaCppEmbedFailed,
    ModelsLlamaCppInvalidGguf,
    ModelsLlamaCppInvalidGgufHtml,
    ModelsModel2vecDownloadFailed,
    ModelsModel2vecEmbedFailed,
    ModelsModel2vecLoadFailed,
    ModelsModelDownloadFailed,
    ModelsQwen37TextEmbeddingApiError,
    ModelsQwen37TextEmbeddingIndexOutOfRange,
    ModelsQwen37TextEmbeddingInvalidIndex,
    ModelsQwen37TextEmbeddingInvalidJson,
    ModelsQwen37TextEmbeddingInvalidVector,
    ModelsQwen37TextEmbeddingMissingApiKey,
    ModelsQwen37TextEmbeddingMissingData,
    ModelsQwen37TextEmbeddingMissingEndpoint,
    ModelsQwen37TextEmbeddingRequestFailed,
    ModelsQwen3VlEmbeddingApiError,
    ModelsQwen3VlEmbeddingIndexOutOfRange,
    ModelsQwen3VlEmbeddingInvalidItem,
    ModelsQwen3VlEmbeddingInvalidJson,
    ModelsQwen3VlEmbeddingInvalidVector,
    ModelsQwen3VlEmbeddingMissingApiKey,
    ModelsQwen3VlEmbeddingMissingEmbeddings,
    ModelsQwen3VlEmbeddingMissingEndpoint,
    ModelsQwen3VlEmbeddingRequestFailed,
    ModelsQwen3VlEmbeddingTooManyImages,
    ModelsQwen3VlEmbeddingUnsupportedImageFormat,
    ModelsQwenTextEmbeddingApiError,
    ModelsQwenTextEmbeddingIndexOutOfRange,
    ModelsQwenTextEmbeddingInvalidIndex,
    ModelsQwenTextEmbeddingInvalidJson,
    ModelsQwenTextEmbeddingInvalidVector,
    ModelsQwenTextEmbeddingMissingApiKey,
    ModelsQwenTextEmbeddingMissingData,
    ModelsQwenTextEmbeddingMissingEndpoint,
    ModelsQwenTextEmbeddingRequestFailed,
    ModelsQwenTextEmbeddingV4ApiError,
    ModelsQwenTextEmbeddingV4IndexOutOfRange,
    ModelsQwenTextEmbeddingV4InvalidIndex,
    ModelsQwenTextEmbeddingV4InvalidJson,
    ModelsQwenTextEmbeddingV4InvalidVector,
    ModelsQwenTextEmbeddingV4MissingApiKey,
    ModelsQwenTextEmbeddingV4MissingData,
    ModelsQwenTextEmbeddingV4MissingEndpoint,
    ModelsQwenTextEmbeddingV4RequestFailed,
    ModelsTransformersJsDisposed,
    ModelsTransformersJsEmbedFailed,
    ModelsTransformersJsInvalidTensor,
    ModelsTransformersJsTokenizationFailed,
    ScannerConfiguredIgnoreReadFailed,
    ScannerOverlappingRootPaths,
    ScannerRootPathStatFailed,
    ScannerUnsupportedRootPath,
    SearchDiagnosisEncodeFailed,
    SearchEmbeddingModelRequired,
    SearchEntityNotFound,
    SearchPlanEmptyRoutes,
    SearchPlanEmptyRouteQuery,
    SearchPlanInvalidFilter,
    SearchPlanInvalidModifiedTimeFilter,
    SearchPlanInvalidModifiedTimeRange,
    SearchPlanInvalidPathFilter,
    ServiceReadSessionClosed,
    StorageCollectionClosed,
    StorageCreateFailed,
    StorageDeleteFailed,
    StorageDocDecodeFailed,
    StorageDocEncodeFailed,
    StorageDocFieldFailed,
    StorageDuplicateFragmentId,
    StorageEntityVectorCountMismatch,
    StorageFileMetaReadOnly,
    StorageForeignTsIndexPresent,
    StorageFragmentFileMismatch,
    StorageInvalidEmbeddingDimension,
    StorageInvalidFragmentGroup,
    StorageInvalidStoragePath,
    StorageMissingEmbeddingSchema,
    StorageReadOnly,
    StorageSchemaFailed,
    StorageUnsupportedStoredContentKind,
    StorageZvecCollectionMissing,
    StorageZvecDeleteFailed,
    StorageZvecFetchFailed,
    StorageZvecFileMetaMissing,
    StorageZvecInitFailed,
    StorageZvecOpenFailed,
    StorageZvecOptimizeFailed,
    StorageZvecQueryFailed,
    StorageZvecUpsertFailed,
    WorkspaceRootUnavailable,
    WorkspaceIndexEmbeddingDimensionMismatch,
    WorkspaceIndexEmbeddingMetricMismatch,
    WorkspaceIndexEmbeddingModelMismatch,
    WorkspaceIndexEmbeddingModelRequired,
    WorkspaceIndexEmbeddingProviderMismatch,
    WorkspaceIndexMissing,
    WorkspaceIndexReadOnly,
    WorkspaceIndexVersionMismatch,
}

impl EngineErrorCode {
    /// The dotted suffix without the `ZVEC_GREP.ENGINE.` prefix.
    ///
    /// `const` so domain error enums can map variants to codes in `const fn`.
    #[must_use]
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::AuthInvalidTarget => "AUTH.INVALID_TARGET",
            Self::AuthRemoteEmbeddingRequired => "AUTH.REMOTE_EMBEDDING_REQUIRED",
            Self::AuthStoreFailed => "AUTH.STORE_FAILED",
            Self::CliAuthorizationDeclined => "CLI.AUTHORIZATION_DECLINED",
            Self::CliAuthorizationRequired => "CLI.AUTHORIZATION_REQUIRED",
            Self::CliConfigInvalid => "CLI.CONFIG_INVALID",
            Self::CliDaemonUnavailable => "CLI.DAEMON_UNAVAILABLE",
            Self::CliInstallRefused => "CLI.INSTALL_REFUSED",
            Self::CliIoFailed => "CLI.IO_FAILED",
            Self::CliNotReady => "CLI.NOT_READY",
            Self::CliRgIncompatible => "CLI.RG_INCOMPATIBLE",
            Self::CliServerIncompatible => "CLI.SERVER_INCOMPATIBLE",
            Self::CliUsage => "CLI.USAGE",
            Self::ConfigEmbeddingEnvironmentInvalid => "CONFIG.EMBEDDING_ENVIRONMENT_INVALID",
            Self::ConfigInvalid => "CONFIG.INVALID",
            Self::ConfigInvalidEmbeddingRuntime => "CONFIG.INVALID_EMBEDDING_RUNTIME",
            Self::ContextEmptyQuery => "CONTEXT.EMPTY_QUERY",
            Self::ContextWorkspaceIndexDisabled => "CONTEXT.WORKSPACE_INDEX_DISABLED",
            Self::ContextWorkspaceIndexNotFound => "CONTEXT.WORKSPACE_INDEX_NOT_FOUND",
            Self::DaemonBlockingJoinFailed => "DAEMON.BLOCKING_JOIN_FAILED",
            Self::DaemonLeaseActive => "DAEMON_LEASE_ACTIVE",
            Self::ExtractorsCodeInvalidChunkOverlap => "EXTRACTORS.CODE_INVALID_CHUNK_OVERLAP",
            Self::ExtractorsCodeInvalidChunkSize => "EXTRACTORS.CODE_INVALID_CHUNK_SIZE",
            Self::ExtractorsEmptyAbsolutePath => "EXTRACTORS.EMPTY_ABSOLUTE_PATH",
            Self::ExtractorsEmptyFileId => "EXTRACTORS.EMPTY_FILE_ID",
            Self::ExtractorsEmptyRelativePath => "EXTRACTORS.EMPTY_RELATIVE_PATH",
            Self::ExtractorsImageEmptyData => "EXTRACTORS.IMAGE_EMPTY_DATA",
            Self::ExtractorsMarkdownInvalidChunkOverlap => {
                "EXTRACTORS.MARKDOWN_INVALID_CHUNK_OVERLAP"
            }
            Self::ExtractorsMarkdownInvalidChunkSize => "EXTRACTORS.MARKDOWN_INVALID_CHUNK_SIZE",
            Self::ExtractorsTextInvalidChunkOverlap => "EXTRACTORS.TEXT_INVALID_CHUNK_OVERLAP",
            Self::ExtractorsTextInvalidChunkSize => "EXTRACTORS.TEXT_INVALID_CHUNK_SIZE",
            Self::FileSelectionTypesUnavailable => "FILE_SELECTION.TYPES_UNAVAILABLE",
            Self::FileSelectionUnknownFileType => "FILE_SELECTION.UNKNOWN_FILE_TYPE",
            Self::IndexingCancelled => "INDEXING.CANCELLED",
            Self::IndexingContentHashFailed => "INDEXING.CONTENT_HASH_FAILED",
            Self::IndexingDeleteFileFailed => "INDEXING.DELETE_FILE_FAILED",
            Self::IndexingEmbeddingFragmentFailed => "INDEXING.EMBEDDING_FRAGMENT_FAILED",
            Self::IndexingEmbeddingThreadFailed => "INDEXING.EMBEDDING_THREAD_FAILED",
            Self::IndexingFilesFailed => "INDEXING.FILES_FAILED",
            Self::IndexingOptimizeFailed => "INDEXING.OPTIMIZE_FAILED",
            Self::IndexingReadSourceFailed => "INDEXING.READ_SOURCE_FAILED",
            Self::IndexingSchedulerFailed => "INDEXING.SCHEDULER_FAILED",
            Self::IndexingStatusFailed => "INDEXING.STATUS_FAILED",
            Self::IndexingWorkspaceFailed => "INDEXING.WORKSPACE_FAILED",
            Self::JsonReadFailed => "JSON.READ_FAILED",
            Self::JsonWriteFailed => "JSON.WRITE_FAILED",
            Self::LexicalEmptyPattern => "LEXICAL.EMPTY_PATTERN",
            Self::LexicalIgnoreFileInvalid => "LEXICAL.IGNORE_FILE_INVALID",
            Self::LexicalInvalidPattern => "LEXICAL.INVALID_PATTERN",
            Self::LexicalPatternFileUnreadable => "LEXICAL.PATTERN_FILE_UNREADABLE",
            Self::LexicalSearchFailed => "LEXICAL.SEARCH_FAILED",
            Self::LexicalUnknownFileType => "LEXICAL.UNKNOWN_FILE_TYPE",
            Self::LockBusy => "LOCK.BUSY",
            Self::LockUnavailable => "LOCK.UNAVAILABLE",
            Self::ManifestDeleteFailed => "MANIFEST.DELETE_FAILED",
            Self::ManifestInvalid => "MANIFEST.INVALID",
            Self::ModelsEmbeddingBackendUnavailable => "MODELS.EMBEDDING_BACKEND_UNAVAILABLE",
            Self::ModelsEmbeddingBatchTooLarge => "MODELS.EMBEDDING_BATCH_TOO_LARGE",
            Self::ModelsEmbeddingCatalogModelNotFound => {
                "MODELS.EMBEDDING_CATALOG_MODEL_NOT_FOUND"
            }
            Self::ModelsEmbeddingDimensionMismatch => "MODELS.EMBEDDING_DIMENSION_MISMATCH",
            Self::ModelsEmbeddingEmptyImage => "MODELS.EMBEDDING_EMPTY_IMAGE",
            Self::ModelsEmbeddingEmptyInput => "MODELS.EMBEDDING_EMPTY_INPUT",
            Self::ModelsEmbeddingEmptyText => "MODELS.EMBEDDING_EMPTY_TEXT",
            Self::ModelsEmbeddingImageTooLarge => "MODELS.EMBEDDING_IMAGE_TOO_LARGE",
            Self::ModelsEmbeddingInvalidTruncatedInputIndex => {
                "MODELS.EMBEDDING_INVALID_TRUNCATED_INPUT_INDEX"
            }
            Self::ModelsEmbeddingModelNotImplemented => "MODELS.EMBEDDING_MODEL_NOT_IMPLEMENTED",
            Self::ModelsEmbeddingNonFiniteVectorValue => {
                "MODELS.EMBEDDING_NON_FINITE_VECTOR_VALUE"
            }
            Self::ModelsEmbeddingUnsupportedContent => "MODELS.EMBEDDING_UNSUPPORTED_CONTENT",
            Self::ModelsEmbeddingVectorCountMismatch => {
                "MODELS.EMBEDDING_VECTOR_COUNT_MISMATCH"
            }
            Self::ModelsLlamaCppDisposed => "MODELS.LLAMA_CPP_DISPOSED",
            Self::ModelsLlamaCppEmbedFailed => "MODELS.LLAMA_CPP_EMBED_FAILED",
            Self::ModelsLlamaCppInvalidGguf => "MODELS.LLAMA_CPP_INVALID_GGUF",
            Self::ModelsLlamaCppInvalidGgufHtml => "MODELS.LLAMA_CPP_INVALID_GGUF_HTML",
            Self::ModelsModel2vecDownloadFailed => "MODELS.MODEL2VEC_DOWNLOAD_FAILED",
            Self::ModelsModel2vecEmbedFailed => "MODELS.MODEL2VEC_EMBED_FAILED",
            Self::ModelsModel2vecLoadFailed => "MODELS.MODEL2VEC_LOAD_FAILED",
            Self::ModelsModelDownloadFailed => "MODELS.MODEL_DOWNLOAD_FAILED",
            Self::ModelsQwen37TextEmbeddingApiError => "MODELS.QWEN37_TEXT_EMBEDDING_API_ERROR",
            Self::ModelsQwen37TextEmbeddingIndexOutOfRange => {
                "MODELS.QWEN37_TEXT_EMBEDDING_INDEX_OUT_OF_RANGE"
            }
            Self::ModelsQwen37TextEmbeddingInvalidIndex => {
                "MODELS.QWEN37_TEXT_EMBEDDING_INVALID_INDEX"
            }
            Self::ModelsQwen37TextEmbeddingInvalidJson => {
                "MODELS.QWEN37_TEXT_EMBEDDING_INVALID_JSON"
            }
            Self::ModelsQwen37TextEmbeddingInvalidVector => {
                "MODELS.QWEN37_TEXT_EMBEDDING_INVALID_VECTOR"
            }
            Self::ModelsQwen37TextEmbeddingMissingApiKey => {
                "MODELS.QWEN37_TEXT_EMBEDDING_MISSING_API_KEY"
            }
            Self::ModelsQwen37TextEmbeddingMissingData => {
                "MODELS.QWEN37_TEXT_EMBEDDING_MISSING_DATA"
            }
            Self::ModelsQwen37TextEmbeddingMissingEndpoint => {
                "MODELS.QWEN37_TEXT_EMBEDDING_MISSING_ENDPOINT"
            }
            Self::ModelsQwen37TextEmbeddingRequestFailed => {
                "MODELS.QWEN37_TEXT_EMBEDDING_REQUEST_FAILED"
            }
            Self::ModelsQwen3VlEmbeddingApiError => "MODELS.QWEN3_VL_EMBEDDING_API_ERROR",
            Self::ModelsQwen3VlEmbeddingIndexOutOfRange => {
                "MODELS.QWEN3_VL_EMBEDDING_INDEX_OUT_OF_RANGE"
            }
            Self::ModelsQwen3VlEmbeddingInvalidItem => "MODELS.QWEN3_VL_EMBEDDING_INVALID_ITEM",
            Self::ModelsQwen3VlEmbeddingInvalidJson => "MODELS.QWEN3_VL_EMBEDDING_INVALID_JSON",
            Self::ModelsQwen3VlEmbeddingInvalidVector => {
                "MODELS.QWEN3_VL_EMBEDDING_INVALID_VECTOR"
            }
            Self::ModelsQwen3VlEmbeddingMissingApiKey => {
                "MODELS.QWEN3_VL_EMBEDDING_MISSING_API_KEY"
            }
            Self::ModelsQwen3VlEmbeddingMissingEmbeddings => {
                "MODELS.QWEN3_VL_EMBEDDING_MISSING_EMBEDDINGS"
            }
            Self::ModelsQwen3VlEmbeddingMissingEndpoint => {
                "MODELS.QWEN3_VL_EMBEDDING_MISSING_ENDPOINT"
            }
            Self::ModelsQwen3VlEmbeddingRequestFailed => {
                "MODELS.QWEN3_VL_EMBEDDING_REQUEST_FAILED"
            }
            Self::ModelsQwen3VlEmbeddingTooManyImages => {
                "MODELS.QWEN3_VL_EMBEDDING_TOO_MANY_IMAGES"
            }
            Self::ModelsQwen3VlEmbeddingUnsupportedImageFormat => {
                "MODELS.QWEN3_VL_EMBEDDING_UNSUPPORTED_IMAGE_FORMAT"
            }
            Self::ModelsQwenTextEmbeddingApiError => "MODELS.QWEN_TEXT_EMBEDDING_API_ERROR",
            Self::ModelsQwenTextEmbeddingIndexOutOfRange => {
                "MODELS.QWEN_TEXT_EMBEDDING_INDEX_OUT_OF_RANGE"
            }
            Self::ModelsQwenTextEmbeddingInvalidIndex => {
                "MODELS.QWEN_TEXT_EMBEDDING_INVALID_INDEX"
            }
            Self::ModelsQwenTextEmbeddingInvalidJson => {
                "MODELS.QWEN_TEXT_EMBEDDING_INVALID_JSON"
            }
            Self::ModelsQwenTextEmbeddingInvalidVector => {
                "MODELS.QWEN_TEXT_EMBEDDING_INVALID_VECTOR"
            }
            Self::ModelsQwenTextEmbeddingMissingApiKey => {
                "MODELS.QWEN_TEXT_EMBEDDING_MISSING_API_KEY"
            }
            Self::ModelsQwenTextEmbeddingMissingData => {
                "MODELS.QWEN_TEXT_EMBEDDING_MISSING_DATA"
            }
            Self::ModelsQwenTextEmbeddingMissingEndpoint => {
                "MODELS.QWEN_TEXT_EMBEDDING_MISSING_ENDPOINT"
            }
            Self::ModelsQwenTextEmbeddingRequestFailed => {
                "MODELS.QWEN_TEXT_EMBEDDING_REQUEST_FAILED"
            }
            Self::ModelsQwenTextEmbeddingV4ApiError => "MODELS.QWEN_TEXT_EMBEDDING_V4_API_ERROR",
            Self::ModelsQwenTextEmbeddingV4IndexOutOfRange => {
                "MODELS.QWEN_TEXT_EMBEDDING_V4_INDEX_OUT_OF_RANGE"
            }
            Self::ModelsQwenTextEmbeddingV4InvalidIndex => {
                "MODELS.QWEN_TEXT_EMBEDDING_V4_INVALID_INDEX"
            }
            Self::ModelsQwenTextEmbeddingV4InvalidJson => {
                "MODELS.QWEN_TEXT_EMBEDDING_V4_INVALID_JSON"
            }
            Self::ModelsQwenTextEmbeddingV4InvalidVector => {
                "MODELS.QWEN_TEXT_EMBEDDING_V4_INVALID_VECTOR"
            }
            Self::ModelsQwenTextEmbeddingV4MissingApiKey => {
                "MODELS.QWEN_TEXT_EMBEDDING_V4_MISSING_API_KEY"
            }
            Self::ModelsQwenTextEmbeddingV4MissingData => {
                "MODELS.QWEN_TEXT_EMBEDDING_V4_MISSING_DATA"
            }
            Self::ModelsQwenTextEmbeddingV4MissingEndpoint => {
                "MODELS.QWEN_TEXT_EMBEDDING_V4_MISSING_ENDPOINT"
            }
            Self::ModelsQwenTextEmbeddingV4RequestFailed => {
                "MODELS.QWEN_TEXT_EMBEDDING_V4_REQUEST_FAILED"
            }
            Self::ModelsTransformersJsDisposed => "MODELS.TRANSFORMERS_JS_DISPOSED",
            Self::ModelsTransformersJsEmbedFailed => "MODELS.TRANSFORMERS_JS_EMBED_FAILED",
            Self::ModelsTransformersJsInvalidTensor => "MODELS.TRANSFORMERS_JS_INVALID_TENSOR",
            Self::ModelsTransformersJsTokenizationFailed => {
                "MODELS.TRANSFORMERS_JS_TOKENIZATION_FAILED"
            }
            Self::ScannerConfiguredIgnoreReadFailed => "SCANNER.CONFIGURED_IGNORE_READ_FAILED",
            Self::ScannerOverlappingRootPaths => "SCANNER.OVERLAPPING_ROOT_PATHS",
            Self::ScannerRootPathStatFailed => "SCANNER.ROOT_PATH_STAT_FAILED",
            Self::ScannerUnsupportedRootPath => "SCANNER.UNSUPPORTED_ROOT_PATH",
            Self::SearchDiagnosisEncodeFailed => "SEARCH.DIAGNOSIS_ENCODE_FAILED",
            Self::SearchEmbeddingModelRequired => "SEARCH.EMBEDDING_MODEL_REQUIRED",
            Self::SearchEntityNotFound => "SEARCH.ENTITY_NOT_FOUND",
            Self::SearchPlanEmptyRoutes => "SEARCH_PLAN.EMPTY_ROUTES",
            Self::SearchPlanEmptyRouteQuery => "SEARCH_PLAN.EMPTY_ROUTE_QUERY",
            Self::SearchPlanInvalidFilter => "SEARCH_PLAN.INVALID_FILTER",
            Self::SearchPlanInvalidModifiedTimeFilter => {
                "SEARCH_PLAN.INVALID_MODIFIED_TIME_FILTER"
            }
            Self::SearchPlanInvalidModifiedTimeRange => {
                "SEARCH_PLAN.INVALID_MODIFIED_TIME_RANGE"
            }
            Self::SearchPlanInvalidPathFilter => "SEARCH_PLAN.INVALID_PATH_FILTER",
            Self::ServiceReadSessionClosed => "SERVICE.READ_SESSION_CLOSED",
            Self::StorageCollectionClosed => "STORAGE.COLLECTION_CLOSED",
            Self::StorageCreateFailed => "STORAGE.CREATE_FAILED",
            Self::StorageDeleteFailed => "STORAGE.DELETE_FAILED",
            Self::StorageDocDecodeFailed => "STORAGE.DOC_DECODE_FAILED",
            Self::StorageDocEncodeFailed => "STORAGE.DOC_ENCODE_FAILED",
            Self::StorageDocFieldFailed => "STORAGE.DOC_FIELD_FAILED",
            Self::StorageDuplicateFragmentId => "STORAGE.DUPLICATE_FRAGMENT_ID",
            Self::StorageEntityVectorCountMismatch => "STORAGE.ENTITY_VECTOR_COUNT_MISMATCH",
            Self::StorageFileMetaReadOnly => "STORAGE.FILE_META_READ_ONLY",
            Self::StorageForeignTsIndexPresent => "STORAGE.FOREIGN_TS_INDEX_PRESENT",
            Self::StorageFragmentFileMismatch => "STORAGE.FRAGMENT_FILE_MISMATCH",
            Self::StorageInvalidEmbeddingDimension => "STORAGE.INVALID_EMBEDDING_DIMENSION",
            Self::StorageInvalidFragmentGroup => "STORAGE.INVALID_FRAGMENT_GROUP",
            Self::StorageInvalidStoragePath => "STORAGE.INVALID_STORAGE_PATH",
            Self::StorageMissingEmbeddingSchema => "STORAGE.MISSING_EMBEDDING_SCHEMA",
            Self::StorageReadOnly => "STORAGE.READ_ONLY",
            Self::StorageSchemaFailed => "STORAGE.SCHEMA_FAILED",
            Self::StorageUnsupportedStoredContentKind => {
                "STORAGE.UNSUPPORTED_STORED_CONTENT_KIND"
            }
            Self::StorageZvecCollectionMissing => "STORAGE.ZVEC_COLLECTION_MISSING",
            Self::StorageZvecDeleteFailed => "STORAGE.ZVEC_DELETE_FAILED",
            Self::StorageZvecFetchFailed => "STORAGE.ZVEC_FETCH_FAILED",
            Self::StorageZvecFileMetaMissing => "STORAGE.ZVEC_FILE_META_MISSING",
            Self::StorageZvecInitFailed => "STORAGE.ZVEC_INIT_FAILED",
            Self::StorageZvecOpenFailed => "STORAGE.ZVEC_OPEN_FAILED",
            Self::StorageZvecOptimizeFailed => "STORAGE.ZVEC_OPTIMIZE_FAILED",
            Self::StorageZvecQueryFailed => "STORAGE.ZVEC_QUERY_FAILED",
            Self::StorageZvecUpsertFailed => "STORAGE.ZVEC_UPSERT_FAILED",
            Self::WorkspaceRootUnavailable => "WORKSPACE.ROOT_UNAVAILABLE",
            Self::WorkspaceIndexEmbeddingDimensionMismatch => {
                "WORKSPACE_INDEX.EMBEDDING_DIMENSION_MISMATCH"
            }
            Self::WorkspaceIndexEmbeddingMetricMismatch => {
                "WORKSPACE_INDEX.EMBEDDING_METRIC_MISMATCH"
            }
            Self::WorkspaceIndexEmbeddingModelMismatch => {
                "WORKSPACE_INDEX.EMBEDDING_MODEL_MISMATCH"
            }
            Self::WorkspaceIndexEmbeddingModelRequired => {
                "WORKSPACE_INDEX.EMBEDDING_MODEL_REQUIRED"
            }
            Self::WorkspaceIndexEmbeddingProviderMismatch => {
                "WORKSPACE_INDEX.EMBEDDING_PROVIDER_MISMATCH"
            }
            Self::WorkspaceIndexMissing => "WORKSPACE_INDEX.MISSING",
            Self::WorkspaceIndexReadOnly => "WORKSPACE_INDEX.READ_ONLY",
            Self::WorkspaceIndexVersionMismatch => "WORKSPACE_INDEX.VERSION_MISMATCH",
        }
    }

    /// The fully-qualified wire string, e.g. `ZVEC_GREP.ENGINE.CONFIG.INVALID`.
    #[must_use]
    pub fn qualified(self) -> String {
        format!("{ENGINE_ERROR_CODE_PREFIX}.{}", self.suffix())
    }

    /// Every engine wire code, one per variant, for the golden registry test
    /// (`tests/golden/error-codes.txt`). Adding a variant without extending
    /// this list fails that test by construction.
    #[must_use]
    pub fn all_codes() -> Vec<Self> {
        vec![
            Self::AuthInvalidTarget,
            Self::AuthRemoteEmbeddingRequired,
            Self::AuthStoreFailed,
            Self::CliAuthorizationDeclined,
            Self::CliAuthorizationRequired,
            Self::CliConfigInvalid,
            Self::CliDaemonUnavailable,
            Self::CliInstallRefused,
            Self::CliIoFailed,
            Self::CliNotReady,
            Self::CliRgIncompatible,
            Self::CliServerIncompatible,
            Self::CliUsage,
            Self::ConfigEmbeddingEnvironmentInvalid,
            Self::ConfigInvalid,
            Self::ConfigInvalidEmbeddingRuntime,
            Self::ContextEmptyQuery,
            Self::ContextWorkspaceIndexDisabled,
            Self::ContextWorkspaceIndexNotFound,
            Self::DaemonBlockingJoinFailed,
            Self::DaemonLeaseActive,
            Self::ExtractorsCodeInvalidChunkOverlap,
            Self::ExtractorsCodeInvalidChunkSize,
            Self::ExtractorsEmptyAbsolutePath,
            Self::ExtractorsEmptyFileId,
            Self::ExtractorsEmptyRelativePath,
            Self::ExtractorsImageEmptyData,
            Self::ExtractorsMarkdownInvalidChunkOverlap,
            Self::ExtractorsMarkdownInvalidChunkSize,
            Self::ExtractorsTextInvalidChunkOverlap,
            Self::ExtractorsTextInvalidChunkSize,
            Self::FileSelectionTypesUnavailable,
            Self::FileSelectionUnknownFileType,
            Self::IndexingCancelled,
            Self::IndexingContentHashFailed,
            Self::IndexingDeleteFileFailed,
            Self::IndexingEmbeddingFragmentFailed,
            Self::IndexingEmbeddingThreadFailed,
            Self::IndexingFilesFailed,
            Self::IndexingOptimizeFailed,
            Self::IndexingReadSourceFailed,
            Self::IndexingSchedulerFailed,
            Self::IndexingStatusFailed,
            Self::IndexingWorkspaceFailed,
            Self::JsonReadFailed,
            Self::JsonWriteFailed,
            Self::LexicalEmptyPattern,
            Self::LexicalIgnoreFileInvalid,
            Self::LexicalInvalidPattern,
            Self::LexicalPatternFileUnreadable,
            Self::LexicalSearchFailed,
            Self::LexicalUnknownFileType,
            Self::LockBusy,
            Self::LockUnavailable,
            Self::ManifestDeleteFailed,
            Self::ManifestInvalid,
            Self::ModelsEmbeddingBackendUnavailable,
            Self::ModelsEmbeddingBatchTooLarge,
            Self::ModelsEmbeddingCatalogModelNotFound,
            Self::ModelsEmbeddingDimensionMismatch,
            Self::ModelsEmbeddingEmptyImage,
            Self::ModelsEmbeddingEmptyInput,
            Self::ModelsEmbeddingEmptyText,
            Self::ModelsEmbeddingImageTooLarge,
            Self::ModelsEmbeddingInvalidTruncatedInputIndex,
            Self::ModelsEmbeddingModelNotImplemented,
            Self::ModelsEmbeddingNonFiniteVectorValue,
            Self::ModelsEmbeddingUnsupportedContent,
            Self::ModelsEmbeddingVectorCountMismatch,
            Self::ModelsLlamaCppDisposed,
            Self::ModelsLlamaCppEmbedFailed,
            Self::ModelsLlamaCppInvalidGguf,
            Self::ModelsLlamaCppInvalidGgufHtml,
            Self::ModelsModel2vecDownloadFailed,
            Self::ModelsModel2vecEmbedFailed,
            Self::ModelsModel2vecLoadFailed,
            Self::ModelsModelDownloadFailed,
            Self::ModelsQwen37TextEmbeddingApiError,
            Self::ModelsQwen37TextEmbeddingIndexOutOfRange,
            Self::ModelsQwen37TextEmbeddingInvalidIndex,
            Self::ModelsQwen37TextEmbeddingInvalidJson,
            Self::ModelsQwen37TextEmbeddingInvalidVector,
            Self::ModelsQwen37TextEmbeddingMissingApiKey,
            Self::ModelsQwen37TextEmbeddingMissingData,
            Self::ModelsQwen37TextEmbeddingMissingEndpoint,
            Self::ModelsQwen37TextEmbeddingRequestFailed,
            Self::ModelsQwen3VlEmbeddingApiError,
            Self::ModelsQwen3VlEmbeddingIndexOutOfRange,
            Self::ModelsQwen3VlEmbeddingInvalidItem,
            Self::ModelsQwen3VlEmbeddingInvalidJson,
            Self::ModelsQwen3VlEmbeddingInvalidVector,
            Self::ModelsQwen3VlEmbeddingMissingApiKey,
            Self::ModelsQwen3VlEmbeddingMissingEmbeddings,
            Self::ModelsQwen3VlEmbeddingMissingEndpoint,
            Self::ModelsQwen3VlEmbeddingRequestFailed,
            Self::ModelsQwen3VlEmbeddingTooManyImages,
            Self::ModelsQwen3VlEmbeddingUnsupportedImageFormat,
            Self::ModelsQwenTextEmbeddingApiError,
            Self::ModelsQwenTextEmbeddingIndexOutOfRange,
            Self::ModelsQwenTextEmbeddingInvalidIndex,
            Self::ModelsQwenTextEmbeddingInvalidJson,
            Self::ModelsQwenTextEmbeddingInvalidVector,
            Self::ModelsQwenTextEmbeddingMissingApiKey,
            Self::ModelsQwenTextEmbeddingMissingData,
            Self::ModelsQwenTextEmbeddingMissingEndpoint,
            Self::ModelsQwenTextEmbeddingRequestFailed,
            Self::ModelsQwenTextEmbeddingV4ApiError,
            Self::ModelsQwenTextEmbeddingV4IndexOutOfRange,
            Self::ModelsQwenTextEmbeddingV4InvalidIndex,
            Self::ModelsQwenTextEmbeddingV4InvalidJson,
            Self::ModelsQwenTextEmbeddingV4InvalidVector,
            Self::ModelsQwenTextEmbeddingV4MissingApiKey,
            Self::ModelsQwenTextEmbeddingV4MissingData,
            Self::ModelsQwenTextEmbeddingV4MissingEndpoint,
            Self::ModelsQwenTextEmbeddingV4RequestFailed,
            Self::ModelsTransformersJsDisposed,
            Self::ModelsTransformersJsEmbedFailed,
            Self::ModelsTransformersJsInvalidTensor,
            Self::ModelsTransformersJsTokenizationFailed,
            Self::ScannerConfiguredIgnoreReadFailed,
            Self::ScannerOverlappingRootPaths,
            Self::ScannerRootPathStatFailed,
            Self::ScannerUnsupportedRootPath,
            Self::SearchDiagnosisEncodeFailed,
            Self::SearchEmbeddingModelRequired,
            Self::SearchEntityNotFound,
            Self::SearchPlanEmptyRoutes,
            Self::SearchPlanEmptyRouteQuery,
            Self::SearchPlanInvalidFilter,
            Self::SearchPlanInvalidModifiedTimeFilter,
            Self::SearchPlanInvalidModifiedTimeRange,
            Self::SearchPlanInvalidPathFilter,
            Self::ServiceReadSessionClosed,
            Self::StorageCollectionClosed,
            Self::StorageCreateFailed,
            Self::StorageDeleteFailed,
            Self::StorageDocDecodeFailed,
            Self::StorageDocEncodeFailed,
            Self::StorageDocFieldFailed,
            Self::StorageDuplicateFragmentId,
            Self::StorageEntityVectorCountMismatch,
            Self::StorageFileMetaReadOnly,
            Self::StorageForeignTsIndexPresent,
            Self::StorageFragmentFileMismatch,
            Self::StorageInvalidEmbeddingDimension,
            Self::StorageInvalidFragmentGroup,
            Self::StorageInvalidStoragePath,
            Self::StorageMissingEmbeddingSchema,
            Self::StorageReadOnly,
            Self::StorageSchemaFailed,
            Self::StorageUnsupportedStoredContentKind,
            Self::StorageZvecCollectionMissing,
            Self::StorageZvecDeleteFailed,
            Self::StorageZvecFetchFailed,
            Self::StorageZvecFileMetaMissing,
            Self::StorageZvecInitFailed,
            Self::StorageZvecOpenFailed,
            Self::StorageZvecOptimizeFailed,
            Self::StorageZvecQueryFailed,
            Self::StorageZvecUpsertFailed,
            Self::WorkspaceRootUnavailable,
            Self::WorkspaceIndexEmbeddingDimensionMismatch,
            Self::WorkspaceIndexEmbeddingMetricMismatch,
            Self::WorkspaceIndexEmbeddingModelMismatch,
            Self::WorkspaceIndexEmbeddingModelRequired,
            Self::WorkspaceIndexEmbeddingProviderMismatch,
            Self::WorkspaceIndexMissing,
            Self::WorkspaceIndexReadOnly,
            Self::WorkspaceIndexVersionMismatch,
        ]
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
#[derive(Debug, Clone)]
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
        write!(f, "{}: {}", self.code, self.message)?;
        if let Some(context) = &self.context {
            write!(f, "\n{context}")?;
        }
        Ok(())
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
/// so the common no-secret path allocates zero times. Mirrors the five passes
/// of the TS implementation (the URL
/// userinfo pass is a hand-rolled scanner because the Rust regex crate has no
/// lookbehind).
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
    static BEARER: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(bearer|basic)\s+[a-z0-9._\-+/=]+")
            .unwrap_or_else(|e| panic!("static redaction regex must compile: {e}"))
    });
    static SK_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\bsk-[a-z0-9_\-]{8,}")
            .unwrap_or_else(|e| panic!("static redaction regex must compile: {e}"))
    });

    // Each pass borrows when it matches nothing, so the common no-secret
    // path allocates zero times; only an actual redaction forces ownership
    // (reassigned solely in the `Owned` arm, where nothing borrows `out`).
    let mut out = redact_url_userinfo(value);
    for (pattern, replacement) in [
        (&KEY_VALUE, "$1\"[redacted]\""),
        (&BEARER, "$1 [redacted]"),
        (&SK_TOKEN, "sk-[redacted]"),
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
        } else {
            let copy_through = (separator + 3).min(bytes.len());
            result.push_str(value.get(cursor..copy_through).unwrap_or(""));
            cursor = copy_through;
        }
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
        let error = EngineError::new(
            EngineErrorCode::JsonReadFailed,
            "failed to read",
        )
        .with_source(cause);
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
}

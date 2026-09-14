//! File preparation and serial commit: extraction, image gating, vector commits, failure marking.

use std::sync::{Arc, Mutex};

use crate::error::{EngineError, EngineErrorCode, EngineResult};
use crate::extraction::vector_content::vector_content_for_fragment;
use crate::extraction::{Source, extract_for_indexing};
use crate::models::{EmbeddingInputKind, EmbeddingModel};
use crate::storage::{FileIndexDiagnostics, IndexedFragment, WorkspaceIndexStorage};
use crate::types::{Content, EntityFragment, FileInfo, ImageFormat};

use super::context::{
    IndexContext, IndexStats, PreparedFile, PreparedFragment, file_context, file_failure_reason,
    is_cancelled_error, throw_if_index_cancelled,
};
use super::input_budget::index_chunk_options;
use super::progress::lock_stats_mut;

pub(crate) fn prepare_file(
    file: &FileInfo,
    ctx: &mut IndexContext<'_>,
) -> EngineResult<Result<PreparedFile, String>> {
    match prepare_file_inner(file, ctx) {
        Ok(prepared) => Ok(Ok(prepared)),
        Err(error) => {
            if is_cancelled_error(&error) {
                return Err(error);
            }
            Ok(Err(mark_file_failed(ctx.storage, file, &error, "prepare")))
        }
    }
}

fn prepare_file_inner(file: &FileInfo, ctx: &IndexContext<'_>) -> EngineResult<PreparedFile> {
    throw_if_index_cancelled(ctx)?;
    let bytes = std::fs::read(&file.absolute_path).map_err(|err| {
        EngineError::new(
            EngineErrorCode::IndexingReadSourceFailed,
            "indexing failed to read source file",
        )
        .with_context(format!("{}\ndetail={err}", file_context(file)))
    })?;
    let text = String::from_utf8_lossy(&bytes);
    let source = if file.kind == crate::types::FileKind::Image {
        Source::Image {
            file,
            data: &bytes,
            format: image_format_of(file)?,
        }
    } else {
        Source::Text {
            file,
            text: text.as_ref(),
        }
    };
    let owned_text: Option<String> = match &source {
        Source::Text { text, .. } => Some((*text).to_owned()),
        Source::Image { .. } => None,
    };
    let chunk_options = index_chunk_options(
        ctx.embedding_model.info().max_input_tokens,
        owned_text.as_deref(),
    );
    let extracted = extract_for_indexing(&source, &chunk_options)?;
    throw_if_index_cancelled(ctx)?;
    let max_chars = chunk_options.max_chunk_chars();
    let fragments: Vec<PreparedFragment> = extracted
        .into_iter()
        .filter(|item| {
            model_accepts_content(ctx.embedding_model.as_ref(), &item.fragment.entity.content)
        })
        .map(|item| {
            let embedding_content = vector_content_for_fragment(
                &item.fragment,
                item.embedding_source.as_ref(),
                Some(max_chars),
            );
            PreparedFragment {
                fragment: item.fragment,
                embedding_content,
            }
        })
        .collect();
    Ok(PreparedFile {
        file: file.clone(),
        fragments,
    })
}

fn image_format_of(file: &FileInfo) -> EngineResult<ImageFormat> {
    match file.format.as_str() {
        "png" => Ok(ImageFormat::Png),
        "jpg" | "jpeg" => Ok(ImageFormat::Jpeg),
        "webp" => Ok(ImageFormat::Webp),
        "gif" => Ok(ImageFormat::Gif),
        other => Err(EngineError::new(
            EngineErrorCode::IndexingReadSourceFailed,
            "indexing found an unsupported image format",
        )
        .with_context(format!("{}\nformat={other}", file_context(file)))),
    }
}

fn model_accepts_content(model: &dyn EmbeddingModel, content: &Content) -> bool {
    let kinds = &model.info().input_kinds;
    match content {
        Content::Text { .. } => kinds.contains(&EmbeddingInputKind::Text),
        Content::Image { .. } => kinds.contains(&EmbeddingInputKind::Image),
    }
}

pub(crate) fn commit_file(
    ctx: &mut IndexContext<'_>,
    prepared: &PreparedFile,
    vectors: &[Vec<f32>],
    truncated_fragment_count: usize,
    stats: &Arc<Mutex<IndexStats>>,
) -> EngineResult<bool> {
    throw_if_index_cancelled(ctx)?;
    if !vectors.is_empty() && prepared.fragments.len() != vectors.len() {
        let error = EngineError::new(
            EngineErrorCode::StorageEntityVectorCountMismatch,
            "embedding returned mismatched entity/vector counts",
        )
        .with_context(format!(
            "fileId={} fragmentCount={} vectorCount={}",
            prepared.file.id.as_str(),
            prepared.fragments.len(),
            vectors.len()
        ));
        let reason = mark_file_failed(ctx.storage, &prepared.file, &error, "commit");
        record_file_failed(&mut lock_stats_mut(stats), &prepared.file, Some(reason));
        return Ok(false);
    }
    let file_vectors: Vec<IndexedFragment> = prepared
        .fragments
        .iter()
        .zip(vectors.iter())
        .map(|(fragment, vector)| IndexedFragment {
            fragment: fragment.fragment.clone(),
            vector: vector.clone(),
        })
        .collect();
    commit_vectors(
        ctx,
        prepared,
        &file_vectors,
        truncated_fragment_count,
        stats,
    )
}

pub(crate) fn commit_vectors(
    ctx: &mut IndexContext<'_>,
    prepared: &PreparedFile,
    file_vectors: &[IndexedFragment],
    truncated_fragment_count: usize,
    stats: &Arc<Mutex<IndexStats>>,
) -> EngineResult<bool> {
    throw_if_index_cancelled(ctx)?;
    match ctx.storage.replace_file(
        &prepared.file,
        file_vectors,
        Some(&FileIndexDiagnostics {
            truncated_fragment_count: Some(truncated_fragment_count),
        }),
    ) {
        Ok(()) => {
            {
                let mut guard = lock_stats_mut(stats);
                guard.files_indexed += 1;
                guard.entities_created += count_public_entities(
                    &prepared
                        .fragments
                        .iter()
                        .map(|fragment| fragment.fragment.clone())
                        .collect::<Vec<_>>(),
                );
            }
            Ok(true)
        }
        Err(error) => {
            if is_cancelled_error(&error) {
                return Err(error);
            }
            let reason = mark_file_failed(ctx.storage, &prepared.file, &error, "commit");
            record_file_failed(&mut lock_stats_mut(stats), &prepared.file, Some(reason));
            Ok(false)
        }
    }
}

pub(crate) fn record_file_failed(stats: &mut IndexStats, file: &FileInfo, reason: Option<String>) {
    stats.files_failed += 1;
    stats.failed_files.push(file.relative_path.clone());
    if let Some(reason) = reason {
        stats
            .failed_file_reasons
            .push(format!("{}: {reason}", file.relative_path));
    }
}

pub(crate) fn mark_file_failed(
    storage: &mut dyn WorkspaceIndexStorage,
    file: &FileInfo,
    error: &EngineError,
    stage: &str,
) -> String {
    let reason = file_failure_reason(stage, error);
    if storage.mark_file_failed(file, &reason).is_err() {
        return file_failure_reason(stage, error);
    }
    reason
}

pub(crate) fn describe_prepared_files(files: &[PreparedFile]) -> String {
    let Some((first, rest)) = files.split_first() else {
        return "0 files".to_owned();
    };
    if rest.is_empty() {
        return first.file.relative_path.clone();
    }
    format!(
        "{} files, starting with {}",
        files.len(),
        first.file.relative_path
    )
}

pub(crate) fn finished_file_detail(succeeded: bool, relative_path: &str) -> String {
    if succeeded {
        format!("indexed {relative_path}")
    } else {
        format!("failed {relative_path}")
    }
}

fn count_public_entities(fragments: &[EntityFragment]) -> usize {
    fragments
        .iter()
        .filter(|fragment| {
            fragment
                .group
                .as_deref()
                .is_none_or(|group| group == fragment.entity.id.as_str())
        })
        .count()
}

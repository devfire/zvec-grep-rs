//! Reciprocal-rank fusion of FTS/vector recall candidates.
//!
//! Port of the `fuseCandidates` family in
//! `engine/pipeline/search/index.ts`: RRF scoring (`1/(60+rank)` per found
//! recall), score-then-id ordering, rank assignment, and hit materialization
//! with sorted evidence and optional traces.

use std::collections::HashSet;

use crate::types::{
    Entity, EntityFragment, FileInfo, SearchFinalTrace, SearchHit, SearchHitEvidence,
    SearchHitTrace, SearchMatchedBy, SearchRecallTrace, SearchStageTrace,
};

/// Recall source: FTS text search or vector search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecallPath {
    Fts,
    Vector,
}

impl RecallPath {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fts => "fts",
            Self::Vector => "vector",
        }
    }
}

/// One recall observation backing a candidate.
#[derive(Debug, Clone)]
pub struct CandidateEvidence {
    pub fragment: EntityFragment,
    pub path: RecallPath,
    pub route_id: Option<String>,
    pub query: Option<String>,
    pub rank: Option<usize>,
    pub score: Option<f64>,
    pub forced: bool,
}

/// One fused entity candidate.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: String,
    pub entity: Entity,
    pub file: FileInfo,
    pub sources: HashSet<RecallPath>,
    pub recall: Vec<SearchRecallTrace>,
    pub evidence: Vec<CandidateEvidence>,
    pub score: f64,
    pub rank: usize,
    pub forced: bool,
}

impl Candidate {
    #[must_use]
    pub fn new(id: String, entity: Entity, file: FileInfo, forced: bool) -> Self {
        Self {
            id,
            entity,
            file,
            sources: HashSet::new(),
            recall: Vec::new(),
            evidence: Vec::new(),
            score: 0.0,
            rank: usize::MAX,
            forced,
        }
    }

    /// Inserts or improves the recall trace for one route (mirrors
    /// `addOrUpdateRecall`).
    pub fn add_or_update_recall(&mut self, recall: SearchRecallTrace) {
        let Some(existing) = self
            .recall
            .iter_mut()
            .find(|item| item.path == recall.path && item.route_id == recall.route_id)
        else {
            self.recall.push(recall);
            return;
        };
        if !recall.found {
            return;
        }
        let improves = !existing.found
            || existing.rank.is_none()
            || recall.rank.is_some_and(|rank| Some(rank) < existing.rank);
        if improves {
            let forced = existing.forced.unwrap_or(false) || recall.forced.unwrap_or(false);
            *existing = recall;
            existing.forced = if forced { Some(true) } else { None };
            return;
        }
        if recall.forced.unwrap_or(false) {
            existing.forced = Some(true);
        }
    }
}

const RRF_K: f64 = 60.0;

/// Scores, orders, and ranks candidates (mirrors `fuseCandidates`).
pub fn fuse_candidates(candidates: &mut [Candidate]) {
    for candidate in candidates.iter_mut() {
        candidate.score = 0.0;
        candidate.forced = candidate
            .recall
            .iter()
            .any(|trace| trace.forced.unwrap_or(false));
        for recall in &candidate.recall {
            if recall.found
                && let Some(rank) = recall.rank
            {
                candidate.score += 1.0 / (RRF_K + rank as f64);
            }
        }
    }
    candidates.sort_by(|left, right| {
        // Total order: RRF scores are finite, and `total_cmp` keeps the
        // comparator consistent (no papered-over NaN arm) even if that
        // invariant ever breaks.
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.id.cmp(&right.id))
    });
    for (index, candidate) in candidates.iter_mut().enumerate() {
        candidate.rank = index + 1;
    }
}

/// Materializes one candidate as a ranked hit (mirrors `candidateToHit`).
#[must_use]
pub fn candidate_to_hit(candidate: &Candidate, limit: usize, trace: bool) -> SearchHit {
    SearchHit {
        entity: candidate.entity.clone(),
        file: candidate.file.clone(),
        evidence: sorted_evidence(&candidate.evidence)
            .into_iter()
            .map(evidence_to_hit_evidence)
            .collect(),
        rank: candidate.rank,
        score: candidate.score,
        matched_by: derive_matched_by(&candidate.sources),
        trace: if trace {
            Some(candidate_to_trace(candidate, limit))
        } else {
            None
        },
    }
}

fn evidence_to_hit_evidence(evidence: &CandidateEvidence) -> SearchHitEvidence {
    SearchHitEvidence {
        range: evidence.fragment.entity.range.clone(),
        content: evidence.fragment.entity.content.clone(),
        metadata: evidence.fragment.entity.metadata.clone(),
        is_entity: evidence.fragment.entity.id.as_str() == public_entity_id(&evidence.fragment),
        path: evidence.path.as_str().to_owned(),
        route_id: evidence.route_id.clone(),
        query: evidence.query.clone(),
        rank: evidence.rank,
        score: evidence.score,
        forced: if evidence.forced { Some(true) } else { None },
    }
}

/// Public identity used for group collapse (mirrors `publicEntityId`).
#[must_use]
pub fn public_entity_id(fragment: &EntityFragment) -> &str {
    fragment
        .group
        .as_deref()
        .unwrap_or_else(|| fragment.entity.id.as_str())
}

fn sorted_evidence(evidence: &[CandidateEvidence]) -> Vec<&CandidateEvidence> {
    let mut sorted: Vec<&CandidateEvidence> = evidence.iter().collect();
    sorted.sort_by(|left, right| {
        left.rank
            .unwrap_or(usize::MAX)
            .cmp(&right.rank.unwrap_or(usize::MAX))
            .then_with(|| left.path.as_str().cmp(right.path.as_str()))
            .then_with(|| {
                left.fragment
                    .entity
                    .id
                    .as_str()
                    .cmp(right.fragment.entity.id.as_str())
            })
    });
    sorted
}

fn candidate_to_trace(candidate: &Candidate, limit: usize) -> SearchHitTrace {
    SearchHitTrace {
        recall: candidate.recall.clone(),
        fusion: Some(SearchStageTrace {
            rank: candidate.rank,
            score: candidate.score,
            forced: if candidate.forced { Some(true) } else { None },
        }),
        ranking: None,
        final_stage: SearchFinalTrace {
            returned_by_limit: candidate.rank <= limit,
            cutoff_rank: limit,
        },
    }
}

fn derive_matched_by(sources: &HashSet<RecallPath>) -> SearchMatchedBy {
    if sources.contains(&RecallPath::Fts) && sources.contains(&RecallPath::Vector) {
        return SearchMatchedBy::FtsVector;
    }
    if sources.contains(&RecallPath::Vector) {
        SearchMatchedBy::Vector
    } else {
        SearchMatchedBy::Fts
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::types::{Content, EntityMetadata};

    fn candidate(id: &str, rank: usize) -> Candidate {
        let mut candidate = Candidate::new(
            id.to_owned(),
            Entity {
                id: crate::ids::EntityId::from_raw(id.to_owned()),
                file_id: crate::ids::FileId::from_raw("f".to_owned()),
                range: crate::types::Range::File,
                content: Content::Text {
                    text: String::new(),
                },
                metadata: None as Option<EntityMetadata>,
            },
            FileInfo {
                id: crate::ids::FileId::from_raw("f".to_owned()),
                absolute_path: "/f".to_owned(),
                relative_path: "f".to_owned(),
                root_path: "/".to_owned(),
                size_bytes: 0,
                last_modified_time: crate::types::UnixMillis::from_millis(0),
                content_hash: None,
                kind: crate::types::FileKind::Text,
                format: crate::types::FileFormat::parse("text"),
                index_status: None,
            },
            false,
        );
        candidate.recall.push(SearchRecallTrace {
            path: "fts".to_owned(),
            found: true,
            rank: Some(rank),
            ..SearchRecallTrace::default()
        });
        candidate
    }

    #[test]
    fn rrf_orders_by_score_then_id() {
        let mut candidates = vec![candidate("b", 2), candidate("a", 2), candidate("c", 1)];
        fuse_candidates(&mut candidates);
        assert_eq!(candidates[0].id, "c");
        assert_eq!(candidates[1].id, "a");
        assert_eq!(candidates[2].id, "b");
        assert_eq!(candidates[0].rank, 1);
    }
}

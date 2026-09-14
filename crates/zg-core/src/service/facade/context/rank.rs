//! Result merging: per-group dedupe plus RRF global ranking with a
//! primary-coverage pass, mirroring TS `selectAndRankContextItems`.

use std::collections::HashMap;

use crate::service::types::{ContextItem, QueryGroupRef, SelectionReason};
use crate::types::SearchMatchedBy;

/// Default result limit, mirroring `DEFAULT_CONTEXT_LIMIT`.
pub const DEFAULT_CONTEXT_LIMIT: usize = 10;

/// Default total result budget across groups, mirroring
/// `DEFAULT_CONTEXT_TOTAL_LIMIT`.
pub const DEFAULT_CONTEXT_TOTAL_LIMIT: usize = 30;

/// Prioritized (coverage + fill) item cap, mirroring
/// `DEFAULT_CONTEXT_PRIORITY_LIMIT`.
const DEFAULT_CONTEXT_PRIORITY_LIMIT: usize = 6;

/// Reciprocal-rank-fusion constant, mirroring `CONTEXT_GROUP_RRF_K`.
const CONTEXT_GROUP_RRF_K: f64 = 60.0;

/// Per-group result limit, mirroring TS `contextGroupLimit`: an explicit
/// limit wins, three or fewer groups share the default, and larger fan-outs
/// split the total budget.
#[must_use]
pub(super) fn context_group_limit(limit: Option<usize>, group_count: usize) -> usize {
    if let Some(limit) = limit {
        return limit;
    }
    if group_count <= 3 {
        return DEFAULT_CONTEXT_LIMIT;
    }
    DEFAULT_CONTEXT_TOTAL_LIMIT.div_ceil(group_count).max(1)
}

/// Dedupe key for an item, mirroring TS `contextItemDedupeKey`: entity id
/// when present, else the absolute path plus the serialized range.
#[must_use]
fn dedupe_key(item: &ContextItem) -> String {
    match &item.entity_id {
        Some(id) => format!("entity:{}", id.as_str()),
        None => match serde_json::to_string(&item.range) {
            Ok(range) => format!("range:{}:{range}", item.file.absolute_path),
            Err(_) => format!("range:{}:", item.file.absolute_path),
        },
    }
}

/// Numeric group order for `Q<n>` ids; unparseable ids sort last, mirroring
/// `Math.min(...[])` yielding `+Infinity` for empty match lists.
#[must_use]
fn group_number(id: &str) -> usize {
    id.strip_prefix('Q')
        .and_then(|number| number.parse().ok())
        .unwrap_or(usize::MAX)
}

/// Merged `matched_by` across one item's group matches, mirroring TS
/// `mergedContextMatchedBy`.
#[must_use]
fn merged_matched_by(matches: &[QueryGroupRef]) -> SearchMatchedBy {
    let has_fts = matches.iter().any(|group_match| {
        matches!(
            group_match.matched_by,
            SearchMatchedBy::Fts | SearchMatchedBy::FtsVector
        )
    });
    let has_vector = matches.iter().any(|group_match| {
        matches!(
            group_match.matched_by,
            SearchMatchedBy::Vector | SearchMatchedBy::FtsVector
        )
    });
    if has_fts && has_vector {
        SearchMatchedBy::FtsVector
    } else if has_fts {
        SearchMatchedBy::Fts
    } else {
        SearchMatchedBy::Vector
    }
}

/// RRF score over an item's group matches: `Σ 1/(60+rank)`.
#[must_use]
fn rrf_score(matches: &[QueryGroupRef]) -> f64 {
    matches
        .iter()
        .map(|group_match| 1.0 / (CONTEXT_GROUP_RRF_K + group_match.rank as f64))
        .sum()
}

/// Best (lowest) group rank across an item's matches.
#[must_use]
fn best_group_rank(matches: &[QueryGroupRef]) -> usize {
    matches
        .iter()
        .map(|group_match| group_match.rank)
        .min()
        .unwrap_or(usize::MAX)
}

/// Lowest group number across an item's matches.
#[must_use]
fn first_group_number(matches: &[QueryGroupRef]) -> usize {
    matches
        .iter()
        .map(|group_match| group_number(&group_match.id))
        .min()
        .unwrap_or(usize::MAX)
}
/// Global item order, mirroring TS `compareContextGlobalRank`: RRF score
/// descending, then best group rank, group number, and dedupe key ascending.
/// The key comparison is byte order, which diverges from TS `localeCompare`
/// for non-ASCII paths (see `docs/ts-divergence.md`).
#[must_use]
fn compare_global(left: &ContextItem, right: &ContextItem) -> std::cmp::Ordering {
    rrf_score(&right.query_groups)
        .partial_cmp(&rrf_score(&left.query_groups))
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| {
            best_group_rank(&left.query_groups).cmp(&best_group_rank(&right.query_groups))
        })
        .then_with(|| {
            first_group_number(&left.query_groups).cmp(&first_group_number(&right.query_groups))
        })
        .then_with(|| dedupe_key(left).cmp(&dedupe_key(right)))
}

/// Group rank of `item` inside `group_id`; absent matches sort last.
#[must_use]
fn rank_in_group(item: &ContextItem, group_id: &str) -> usize {
    item.query_groups
        .iter()
        .find(|group_match| group_match.id == group_id)
        .map_or(usize::MAX, |group_match| group_match.rank)
}

/// Merges per-group items into the final ranked list, mirroring TS
/// `selectAndRankContextItems`: dedupe by entity/range (merging group
/// matches with min rank per group), primary-group coverage pass, global
/// fill to the priority cap, then the unprioritized tail — renumbered 1..n.
/// Surviving `score` is whichever group's instance came first while
/// `matched_by` is merged, exactly like TS.
#[must_use]
pub(super) fn select_and_rank(
    items: Vec<ContextItem>,
    coverage_group_ids: &[String],
) -> Vec<ContextItem> {
    let mut merged: Vec<ContextItem> = Vec::with_capacity(items.len());
    let mut position_by_key: HashMap<String, usize> = HashMap::new();
    for item in items {
        let key = dedupe_key(&item);
        if let Some(&position) = position_by_key.get(&key) {
            if let Some(existing) = merged.get_mut(position) {
                for group_match in item.query_groups {
                    if let Some(known) = existing
                        .query_groups
                        .iter_mut()
                        .find(|known| known.id == group_match.id)
                    {
                        if group_match.rank < known.rank {
                            known.rank = group_match.rank;
                        }
                    } else {
                        existing.query_groups.push(group_match);
                    }
                }
                existing
                    .query_groups
                    .sort_by_key(|group_match| group_number(&group_match.id));
                existing.matched_by = Some(
                    merged_matched_by(&existing.query_groups)
                        .as_str()
                        .to_owned(),
                );
            }
            continue;
        }
        position_by_key.insert(key, merged.len());
        merged.push(item);
    }
    merged.sort_by(compare_global);
    let mut prioritized: Vec<ContextItem> = Vec::new();
    for coverage_id in coverage_group_ids {
        if prioritized.len() >= DEFAULT_CONTEXT_PRIORITY_LIMIT {
            break;
        }
        let best = merged
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                item.query_groups
                    .iter()
                    .any(|group_match| group_match.id == *coverage_id)
            })
            .min_by(|(_, left), (_, right)| {
                rank_in_group(left, coverage_id)
                    .cmp(&rank_in_group(right, coverage_id))
                    .then_with(|| compare_global(left, right))
            })
            .map(|(position, _)| position);
        if let Some(position) = best {
            let mut item = merged.remove(position);
            item.selection_reason = Some(SelectionReason::Coverage);
            item.coverage_group = Some(coverage_id.clone());
            prioritized.push(item);
        }
    }
    while prioritized.len() < DEFAULT_CONTEXT_PRIORITY_LIMIT && !merged.is_empty() {
        let mut item = merged.remove(0);
        item.selection_reason = Some(SelectionReason::GlobalFill);
        prioritized.push(item);
    }
    prioritized.extend(merged);
    for (index, item) in prioritized.iter_mut().enumerate() {
        item.rank = index + 1;
    }
    prioritized
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::ids::EntityId;
    use crate::service::types::{
        ContentStatus, ContextFile, ContextItemKind, GroupRole, QueryGroupRef,
    };
    use crate::types::{Content, Range};

    #[test]
    fn group_limit_divides_total_budget() {
        assert_eq!(context_group_limit(Some(50), 4), 50);
        assert_eq!(context_group_limit(None, 1), DEFAULT_CONTEXT_LIMIT);
        assert_eq!(context_group_limit(None, 3), DEFAULT_CONTEXT_LIMIT);
        assert_eq!(context_group_limit(None, 4), 8);
        assert_eq!(context_group_limit(None, 10), 3);
        assert_eq!(context_group_limit(None, 0), DEFAULT_CONTEXT_LIMIT);
        assert_eq!(context_group_limit(None, 100), 1);
    }

    fn merge_item(
        entity_suffix: u8,
        group_id: &str,
        group_query: &str,
        rank: usize,
        matched_by: SearchMatchedBy,
    ) -> ContextItem {
        ContextItem {
            kind: ContextItemKind::IndexedEntity,
            rank,
            file: ContextFile {
                absolute_path: "/repo/a.rs".to_owned(),
                relative_path: "a.rs".to_owned(),
                root_path: "/repo".to_owned(),
            },
            range: Range::Text {
                start_line: 1,
                end_line: 2,
                start_offset: 0,
                end_offset: 10,
            },
            excerpt_range: None,
            content: Content::Text {
                text: "fn f() {}".to_owned(),
            },
            content_role: None,
            outline: None,
            status: ContentStatus::Fresh,
            score: Some(1.0 / rank as f64),
            matched_by: Some(matched_by.as_str().to_owned()),
            metadata: None,
            entity_id: Some(
                EntityId::parse(&format!("{entity_suffix:064}")).expect("valid entity id"),
            ),
            trace: None,
            query_groups: vec![QueryGroupRef {
                id: group_id.to_owned(),
                query: group_query.to_owned(),
                role: GroupRole::Primary,
                rank,
                matched_by,
            }],
            container: None,
            selection_reason: None,
            coverage_group: None,
        }
    }

    #[test]
    fn merge_dedupes_coverage_and_fill() {
        use SearchMatchedBy::{Fts, Vector};
        let items = vec![
            merge_item(1, "Q1", "a", 2, Fts),
            merge_item(1, "Q2", "b", 1, Vector),
            merge_item(2, "Q1", "a", 1, Fts),
            merge_item(3, "Q2", "b", 2, Fts),
            merge_item(4, "Q1", "a", 3, Fts),
            merge_item(5, "Q1", "a", 4, Fts),
            merge_item(6, "Q1", "a", 5, Fts),
            merge_item(7, "Q1", "a", 6, Fts),
        ];
        let ranked = select_and_rank(items, &["Q1".to_owned(), "Q2".to_owned()]);
        assert_eq!(ranked.len(), 7);
        for (index, item) in ranked.iter().enumerate() {
            assert_eq!(item.rank, index + 1);
        }
        assert!(
            ranked[0]
                .entity_id
                .as_ref()
                .is_some_and(|id| id.as_str().ends_with('2'))
        );
        assert_eq!(ranked[0].selection_reason, Some(SelectionReason::Coverage));
        assert_eq!(ranked[0].coverage_group.as_deref(), Some("Q1"));
        let merged = &ranked[1];
        assert_eq!(merged.query_groups.len(), 2);
        assert_eq!(merged.matched_by.as_deref(), Some("fts+vector"));
        assert_eq!(merged.selection_reason, Some(SelectionReason::Coverage));
        assert_eq!(merged.coverage_group.as_deref(), Some("Q2"));
        for item in &ranked[2..6] {
            assert_eq!(item.selection_reason, Some(SelectionReason::GlobalFill));
        }
        assert_eq!(ranked[6].selection_reason, None);
    }
    #[test]
    fn groupless_items_sort_last() {
        use SearchMatchedBy::Fts;
        let mut grouped = merge_item(1, "Q9", "a", 50, Fts);
        grouped.entity_id = None;
        let mut groupless = merge_item(2, "Q1", "a", 1, Fts);
        groupless.entity_id = None;
        groupless.query_groups.clear();
        groupless.range = Range::Text {
            start_line: 9,
            end_line: 10,
            start_offset: 0,
            end_offset: 10,
        };
        let ranked = select_and_rank(vec![groupless, grouped], &[]);
        assert_eq!(ranked[0].query_groups.len(), 1);
        assert!(ranked[1].query_groups.is_empty());
    }
}

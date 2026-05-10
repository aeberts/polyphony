use std::collections::{HashMap, HashSet};

use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};
use polyphony_core::{InboxItemRow, RuntimeSnapshot};

#[derive(Clone, Copy)]
pub(crate) struct DisplayRow {
    pub item_idx: usize,
    pub depth: u8,
    pub last_child: bool,
}

pub(crate) fn display_rows(snapshot: &RuntimeSnapshot) -> Vec<DisplayRow> {
    let issues = &snapshot.inbox_items;
    let mut indices = (0..issues.len()).collect::<Vec<_>>();
    indices.sort_by(
        |&a, &b| match (issues[a].created_at, issues[b].created_at) {
            (Some(a_t), Some(b_t)) => a_t.cmp(&b_t),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => issues[a].identifier.cmp(&issues[b].identifier),
        },
    );

    let mut children_by_parent: HashMap<&str, Vec<usize>> = HashMap::new();
    for &idx in &indices {
        if let Some(parent_id) = &issues[idx].parent_id {
            children_by_parent
                .entry(parent_id.as_str())
                .or_default()
                .push(idx);
        }
    }
    for child_indices in children_by_parent.values_mut() {
        child_indices.sort_by(|&a, &b| issues[a].identifier.cmp(&issues[b].identifier));
    }

    if children_by_parent.is_empty() {
        return indices
            .into_iter()
            .map(|item_idx| DisplayRow {
                item_idx,
                depth: 0,
                last_child: false,
            })
            .collect();
    }

    let child_set = children_by_parent
        .values()
        .flat_map(|children| children.iter().copied())
        .collect::<HashSet<_>>();
    let visible_parents = indices
        .iter()
        .filter(|&&idx| children_by_parent.contains_key(issues[idx].item_id.as_str()))
        .map(|&idx| issues[idx].item_id.as_str())
        .collect::<HashSet<_>>();
    let mut rows = Vec::with_capacity(indices.len());
    for &idx in &indices {
        if child_set.contains(&idx)
            && let Some(parent_id) = &issues[idx].parent_id
            && visible_parents.contains(parent_id.as_str())
        {
            continue;
        }
        rows.push(DisplayRow {
            item_idx: idx,
            depth: 0,
            last_child: false,
        });
        if let Some(children) = children_by_parent.get(issues[idx].item_id.as_str()) {
            for (child_idx, &idx) in children.iter().enumerate() {
                rows.push(DisplayRow {
                    item_idx: idx,
                    depth: 1,
                    last_child: child_idx == children.len() - 1,
                });
            }
        }
    }
    rows
}

pub(crate) fn display_rows_matching(snapshot: &RuntimeSnapshot, query: &str) -> Vec<DisplayRow> {
    let rows = display_rows(snapshot);
    let query = query.trim();
    if query.is_empty() {
        return rows;
    }

    let matcher = SkimMatcherV2::default().smart_case();
    let mut matches = rows
        .into_iter()
        .filter_map(|row| {
            let item = snapshot.inbox_items.get(row.item_idx)?;
            let haystack = searchable_text(snapshot, item);
            matcher
                .fuzzy_match(&haystack, query)
                .map(|score| (score, row))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|(a_score, a_row), (b_score, b_row)| {
        b_score
            .cmp(a_score)
            .then_with(|| a_row.item_idx.cmp(&b_row.item_idx))
    });
    matches.into_iter().map(|(_, row)| row).collect()
}

fn searchable_text(snapshot: &RuntimeSnapshot, item: &InboxItemRow) -> String {
    let mut fields = vec![
        item.repo_id.clone(),
        item.item_id.clone(),
        item.source.clone(),
        item.identifier.clone(),
        item.title.clone(),
        item.status.clone(),
    ];

    if let Some(description) = item.description.as_deref() {
        fields.push(description.to_string());
    }
    if let Some(url) = item.url.as_deref() {
        fields.push(url.to_string());
    }
    if let Some(author) = item.author.as_deref() {
        fields.push(author.to_string());
    }
    if let Some(parent_id) = item.parent_id.as_deref() {
        fields.push(parent_id.to_string());
    }
    fields.extend(item.labels.iter().cloned());

    for run in snapshot
        .runs
        .iter()
        .filter(|run| run.issue_identifier.as_deref() == Some(item.identifier.as_str()))
    {
        fields.push(run.title.clone());
        fields.push(run.status.to_string());
    }

    fields.join(" ")
}

pub(crate) fn hierarchy_prefix(depth: u8, last_child: bool) -> &'static str {
    if depth == 0 {
        ""
    } else if last_child {
        "└── "
    } else {
        "├── "
    }
}

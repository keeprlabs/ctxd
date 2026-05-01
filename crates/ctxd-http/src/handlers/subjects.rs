//! `GET /v1/subjects/tree` — subject hierarchy with per-node counts.

use crate::router::AppState;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use ctxd_core::subject::Subject;
use serde::{Deserialize, Serialize};

/// Query params.
#[derive(Debug, Deserialize)]
pub(crate) struct TreeQuery {
    /// Optional prefix. Defaults to `/` (the whole tree).
    prefix: Option<String>,
}

/// One node in the subject tree.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SubjectNode {
    /// Full subject path of this node (e.g. `/work/notes`). Full paths
    /// are cheaper for the client than basename + position because the
    /// JS code can use them as URL fragments without a join step.
    pub name: String,
    /// Number of events whose subject equals this node OR is a descendant.
    pub count: u64,
    /// Direct children, sorted alphabetically.
    pub children: Vec<SubjectNode>,
}

/// `GET /v1/subjects/tree?prefix=P` — return the subject tree shaped
/// for direct rendering by the dashboard's subjects view.
///
/// The store gives us a flat `(subject, count)` list under the prefix.
/// We fold that into a tree where each node's count includes its
/// descendants — i.e. counts are *cumulative*, not just direct hits.
/// This matches the dashboard's expected display: clicking `/work`
/// should show "800 events" (everything beneath), not "0" (no
/// events with subject literally equal to `/work`).
#[tracing::instrument(skip(state))]
pub(crate) async fn subject_tree(
    State(state): State<AppState>,
    Query(q): Query<TreeQuery>,
) -> Result<Json<SubjectNode>, (StatusCode, String)> {
    let prefix_str = q.prefix.as_deref().unwrap_or("/");
    let prefix = Subject::new(prefix_str)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid prefix: {e}")))?;

    let counts = state
        .store
        .subject_counts(Some(&prefix))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let root_path = prefix.as_str().to_string();
    Ok(Json(build_tree(&root_path, &counts)))
}

/// Fold `(subject, n)` rows into a cumulative-count tree rooted at
/// `root`.
fn build_tree(root: &str, rows: &[(Subject, u64)]) -> SubjectNode {
    // Step 1: build a map from full path → direct count.
    use std::collections::BTreeMap;
    let mut direct: BTreeMap<String, u64> = BTreeMap::new();
    for (sub, n) in rows {
        direct.insert(sub.as_str().to_string(), *n);
    }

    // Step 2: walk every path and split into segments to materialize
    // intermediate nodes (e.g. `/work/notes/a` implies `/work` and
    // `/work/notes` exist as parents even if no event sits there).
    let mut all_paths: BTreeMap<String, u64> = BTreeMap::new();
    for (sub, n) in rows {
        let s = sub.as_str();
        // Walk up from this leaf to `root`, accumulating the count at
        // each ancestor at-or-under the requested root.
        let segments: Vec<&str> = s
            .strip_prefix('/')
            .unwrap_or(s)
            .split('/')
            .filter(|p| !p.is_empty())
            .collect();
        let mut acc = String::new();
        for seg in &segments {
            acc.push('/');
            acc.push_str(seg);
            let in_scope = root == "/" || acc == root || acc.starts_with(&format!("{root}/"));
            if in_scope {
                *all_paths.entry(acc.clone()).or_insert(0) += n;
            }
        }
        // The "/" root never appears as a path segment, so credit it
        // explicitly when the caller asked for the whole tree.
        if root == "/" {
            *all_paths.entry("/".into()).or_insert(0) += n;
        }
    }

    // Step 3: build nodes recursively from the path map.
    fn collect_children(parent: &str, paths: &BTreeMap<String, u64>) -> Vec<SubjectNode> {
        let mut children = Vec::new();
        let parent_prefix = if parent == "/" {
            "/".to_string()
        } else {
            format!("{parent}/")
        };
        // Direct children of `parent` are paths exactly one level deeper.
        for path in paths.keys() {
            if !path.starts_with(&parent_prefix) || path == parent {
                continue;
            }
            let remainder = &path[parent_prefix.len()..];
            if remainder.contains('/') {
                continue; // grandchild, skip
            }
            let count = *paths.get(path).unwrap_or(&0);
            let descendants = collect_children(path, paths);
            children.push(SubjectNode {
                name: path.clone(),
                count,
                children: descendants,
            });
        }
        children
    }

    let count = direct
        .get(root)
        .copied()
        .unwrap_or(*all_paths.get(root).unwrap_or(&0));
    let children = collect_children(root, &all_paths);
    // Recompute root count as the sum if we don't have a direct value
    // (the root is often a synthetic node).
    let cumulative = all_paths.get(root).copied().unwrap_or(count);

    SubjectNode {
        name: root.to_string(),
        count: cumulative,
        children,
    }
}

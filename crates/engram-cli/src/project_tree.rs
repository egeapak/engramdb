//! Pure, presentation-independent layout for `engramdb projects list`.
//!
//! [`build_render_model`] turns the flat `Vec<ProjectListOutput>` (registry
//! order) into an ordered [`RenderLine`] sequence that the formatter prints.
//! All of the interesting logic lives here — building the worktree tree,
//! grouping roots under filesystem-directory headers, sorting, and applying
//! the [`ProjectListGrouping`] mode — so it can be unit-tested without any
//! terminal, color, or I/O. `output.rs` only maps [`RenderLine`] to strings.
//!
//! Invariants that hold in *every* mode:
//! - Worktree sub-projects nest under their real parent (`parent_project_id`),
//!   not under whatever entry happens to precede them in registry order.
//! - Dangling / orphaned links are promoted back to roots; `parent_project_id`
//!   cycles are broken deterministically so traversal always terminates.
//! - Everything is sorted by path, case-insensitively.
//!
//! The mode only controls the *directory headers* (see [`ProjectListGrouping`]).

use crate::output::ProjectListOutput;
use engramdb::types::ProjectListGrouping;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// One line of rendered project-list output, before styling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderLine {
    /// A blank spacer line between blocks.
    Blank,
    /// A filesystem-directory header (`Always`, or `Auto` for ≥2 projects).
    Header(String),
    /// A single project row.
    Project {
        /// Full project id (the formatter shortens it).
        project_id: String,
        /// Nesting depth: 0 = a directory-level root, ≥1 = a worktree child.
        depth: usize,
        /// Whether this row sits under a [`RenderLine::Header`] (adds a base
        /// indent so rows align beneath the header).
        under_header: bool,
        /// The path text to show: a basename under a header, otherwise the
        /// full path (see module invariants and the per-mode rules).
        label: String,
        /// Whether the project still exists on disk (`ok` vs `missing`).
        exists: bool,
    },
}

/// A project and its (already-sorted) worktree descendants.
struct TreeNode<'a> {
    entry: &'a ProjectListOutput,
    children: Vec<TreeNode<'a>>,
}

/// Case-insensitive ordering with an exact-bytes tiebreak (so the result is
/// total and deterministic regardless of input order).
fn ci_cmp(a: &str, b: &str) -> Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

/// The containing directory of `path`, used as a group header. Falls back to
/// the path itself for degenerate inputs (a filesystem root, or a bare
/// relative component) so grouping never panics.
fn dir_of(path: &str) -> String {
    match Path::new(path).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_string_lossy().into_owned(),
        _ => path.to_string(),
    }
}

/// The last path component of `path`, or the whole string if it has none.
fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Label for a nested worktree child. Basename when the child lives directly
/// inside — or as a sibling of — its parent project (the common worktree
/// layouts, where the nesting already makes the relationship obvious). Full
/// path when the worktree lives somewhere unrelated, so it stays unambiguous.
fn child_label(child_path: &str, parent_path: &str) -> String {
    let cp = Path::new(child_path);
    let pp = Path::new(parent_path);
    let same_dir = cp.parent() == pp.parent();
    let direct_child = cp.parent() == Some(pp);
    if same_dir || direct_child {
        basename(child_path)
    } else {
        child_path.to_string()
    }
}

/// The effective parent id of `entry`: its `parent_project_id`, but only when
/// that id refers to another present entry (not itself, not a dangling link).
fn effective_parent<'a>(
    entry: &'a ProjectListOutput,
    by_id: &HashMap<&str, &ProjectListOutput>,
) -> Option<&'a str> {
    match entry.parent_project_id.as_deref() {
        Some(p) if p != entry.project_id.as_str() && by_id.contains_key(p) => Some(p),
        _ => None,
    }
}

/// Recursively assemble a [`TreeNode`], sorting children by path and guarding
/// against `parent_project_id` cycles via `visited`.
fn build_node<'a>(
    entry: &'a ProjectListOutput,
    children_of: &HashMap<&'a str, Vec<&'a ProjectListOutput>>,
    visited: &mut HashSet<&'a str>,
) -> TreeNode<'a> {
    visited.insert(entry.project_id.as_str());
    let mut kids: Vec<&ProjectListOutput> = children_of
        .get(entry.project_id.as_str())
        .cloned()
        .unwrap_or_default();
    kids.sort_by(|a, b| ci_cmp(&a.project_path, &b.project_path));
    let mut children = Vec::new();
    for kid in kids {
        if !visited.contains(kid.project_id.as_str()) {
            children.push(build_node(kid, children_of, visited));
        }
    }
    TreeNode { entry, children }
}

/// Emit a node and its subtree.
///
/// - `depth` 0 is a directory-level root; deeper rows are worktree children.
/// - `under_header` adds the header base indent.
/// - `root_full_path` shows the full path for a depth-0 root (inline / `none`
///   rendering) instead of its basename (headered rendering).
/// - `force_full` (the `none` mode) shows the full path for *every* row.
fn emit_tree(
    node: &TreeNode,
    depth: usize,
    under_header: bool,
    parent_path: Option<&str>,
    root_full_path: bool,
    force_full: bool,
    out: &mut Vec<RenderLine>,
) {
    let path = node.entry.project_path.as_str();
    let label = if force_full {
        path.to_string()
    } else if depth == 0 {
        if root_full_path {
            path.to_string()
        } else {
            basename(path)
        }
    } else {
        child_label(
            path,
            parent_path.expect("non-root node always has a parent path"),
        )
    };
    out.push(RenderLine::Project {
        project_id: node.entry.project_id.clone(),
        depth,
        under_header,
        label,
        exists: node.entry.exists,
    });
    for child in &node.children {
        emit_tree(
            child,
            depth + 1,
            under_header,
            Some(path),
            root_full_path,
            force_full,
            out,
        );
    }
}

/// Build the ordered render model for a project list under the given grouping.
pub fn build_render_model(
    entries: &[ProjectListOutput],
    mode: ProjectListGrouping,
) -> Vec<RenderLine> {
    let by_id: HashMap<&str, &ProjectListOutput> =
        entries.iter().map(|e| (e.project_id.as_str(), e)).collect();

    // parent id -> its children, in registry order (re-sorted in build_node).
    let mut children_of: HashMap<&str, Vec<&ProjectListOutput>> = HashMap::new();
    for e in entries {
        if let Some(parent) = effective_parent(e, &by_id) {
            children_of.entry(parent).or_default().push(e);
        }
    }

    // Roots: true roots first (input order), then any node still unvisited —
    // those participate in a cycle and are promoted to roots to break it.
    let mut visited: HashSet<&str> = HashSet::new();
    let mut roots: Vec<TreeNode> = Vec::new();
    for e in entries {
        if effective_parent(e, &by_id).is_none() && !visited.contains(e.project_id.as_str()) {
            roots.push(build_node(e, &children_of, &mut visited));
        }
    }
    for e in entries {
        if !visited.contains(e.project_id.as_str()) {
            roots.push(build_node(e, &children_of, &mut visited));
        }
    }

    let mut out: Vec<RenderLine> = Vec::new();

    // `none`: one flat, path-sorted list; no headers; every row full-path.
    if mode == ProjectListGrouping::None {
        roots.sort_by(|a, b| ci_cmp(&a.entry.project_path, &b.entry.project_path));
        for node in &roots {
            emit_tree(node, 0, false, None, true, true, &mut out);
        }
        return out;
    }

    // Group roots by containing directory, sorted by dir then by path so each
    // header's rows come out in path order.
    let mut dir_roots: Vec<(String, TreeNode)> = roots
        .into_iter()
        .map(|n| (dir_of(&n.entry.project_path), n))
        .collect();
    dir_roots.sort_by(|(da, na), (db, nb)| {
        ci_cmp(da, db).then_with(|| ci_cmp(&na.entry.project_path, &nb.entry.project_path))
    });

    let mut emitted_any = false;
    let mut prev_was_header = false;
    let mut i = 0;
    while i < dir_roots.len() {
        let dir = dir_roots[i].0.clone();
        let mut j = i + 1;
        while j < dir_roots.len() && dir_roots[j].0 == dir {
            j += 1;
        }
        let group = &dir_roots[i..j];
        // `always`: header for every folder. `auto`: header only for ≥2.
        let use_header = mode == ProjectListGrouping::Always
            || (mode == ProjectListGrouping::Auto && group.len() >= 2);

        if use_header {
            if emitted_any {
                out.push(RenderLine::Blank);
            }
            out.push(RenderLine::Header(dir));
            for (_, node) in group {
                emit_tree(node, 0, true, None, false, false, &mut out);
            }
        } else {
            // Inline (an `auto` singleton folder): one full-path row, no header.
            if emitted_any && prev_was_header {
                out.push(RenderLine::Blank);
            }
            for (_, node) in group {
                emit_tree(node, 0, false, None, true, false, &mut out);
            }
        }
        emitted_any = true;
        prev_was_header = use_header;
        i = j;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, path: &str, parent: Option<&str>) -> ProjectListOutput {
        ProjectListOutput {
            project_id: id.to_string(),
            project_path: path.to_string(),
            exists: true,
            parent_project_id: parent.map(|s| s.to_string()),
        }
    }

    /// Convenience: the ordered `(depth, label)` of every Project row.
    fn projects(lines: &[RenderLine]) -> Vec<(usize, String)> {
        lines
            .iter()
            .filter_map(|l| match l {
                RenderLine::Project { depth, label, .. } => Some((*depth, label.clone())),
                _ => None,
            })
            .collect()
    }

    fn headers(lines: &[RenderLine]) -> Vec<String> {
        lines
            .iter()
            .filter_map(|l| match l {
                RenderLine::Header(h) => Some(h.clone()),
                _ => None,
            })
            .collect()
    }

    /// A worktree nests under its real parent even when an unrelated project
    /// sits between them in registry order — the old bug.
    #[test]
    fn worktree_nests_under_real_parent_not_neighbor() {
        let entries = vec![
            entry("par", "/proj/parent", None),
            entry("unrel", "/proj/unrelated", None),
            entry("wt", "/proj/parent-wt", Some("par")),
        ];
        // Always mode groups everything under /proj.
        let model = build_render_model(&entries, ProjectListGrouping::Always);
        assert_eq!(headers(&model), vec!["/proj".to_string()]);
        // parent (depth 0), then its worktree (depth 1), then unrelated (0).
        assert_eq!(
            projects(&model),
            vec![
                (0, "parent".to_string()),
                (1, "parent-wt".to_string()),
                (0, "unrelated".to_string()),
            ]
        );
    }

    /// A worktree whose parent id is absent from the list is promoted to root.
    #[test]
    fn orphan_worktree_promoted_to_root() {
        let entries = vec![entry("wt", "/proj/child", Some("ghost"))];
        let model = build_render_model(&entries, ProjectListGrouping::Always);
        // Rendered as a normal root, not swallowed.
        assert_eq!(projects(&model), vec![(0, "child".to_string())]);
    }

    /// A `parent_project_id` cycle terminates and emits both nodes exactly once.
    #[test]
    fn parent_cycle_is_broken_and_terminates() {
        let entries = vec![
            entry("a", "/proj/a", Some("b")),
            entry("b", "/proj/b", Some("a")),
        ];
        let model = build_render_model(&entries, ProjectListGrouping::None);
        let ids: Vec<String> = model
            .iter()
            .filter_map(|l| match l {
                RenderLine::Project { project_id, .. } => Some(project_id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(ids.len(), 2, "both cycle members appear exactly once");
        assert!(ids.contains(&"a".to_string()));
        assert!(ids.contains(&"b".to_string()));
    }

    /// Grandchild worktrees nest recursively.
    #[test]
    fn grandchild_nests_recursively() {
        let entries = vec![
            entry("p", "/proj/p", None),
            entry("c", "/proj/p/c", Some("p")),
            entry("g", "/proj/p/c/g", Some("c")),
        ];
        // `none` mode shows the full path on every row, including children.
        let model = build_render_model(&entries, ProjectListGrouping::None);
        assert_eq!(
            projects(&model),
            vec![
                (0, "/proj/p".to_string()),
                (1, "/proj/p/c".to_string()),
                (2, "/proj/p/c/g".to_string()),
            ]
        );
    }

    /// Groups and within-group rows are path-sorted, case-insensitively.
    #[test]
    fn groups_and_rows_sorted_case_insensitively() {
        let entries = vec![
            entry("1", "/a/ceiba/Zeta", None),
            entry("2", "/a/ceiba/alpha", None),
            entry("3", "/a/ceiba/MDR", None),
            entry("4", "/a/Beta/one", None),
        ];
        let model = build_render_model(&entries, ProjectListGrouping::Always);
        // /a/Beta sorts before /a/ceiba (case-insensitive).
        assert_eq!(
            headers(&model),
            vec!["/a/Beta".to_string(), "/a/ceiba".to_string()]
        );
        // Within /a/ceiba: alpha, MDR, Zeta (case-insensitive).
        assert_eq!(
            projects(&model),
            vec![
                (0, "one".to_string()),
                (0, "alpha".to_string()),
                (0, "MDR".to_string()),
                (0, "Zeta".to_string()),
            ]
        );
    }

    /// `auto`: a folder with one project renders inline (full path, no header);
    /// a folder with ≥2 gets a header with basenames.
    #[test]
    fn auto_headers_only_multi_project_folders() {
        let entries = vec![
            entry("solo", "/x/only/solo", None),
            entry("m1", "/x/many/a", None),
            entry("m2", "/x/many/b", None),
        ];
        let model = build_render_model(&entries, ProjectListGrouping::Auto);
        // Only the multi-project folder gets a header.
        assert_eq!(headers(&model), vec!["/x/many".to_string()]);
        // Many → basenames under the header; solo folder → inline full path.
        assert_eq!(
            projects(&model),
            vec![
                (0, "a".to_string()),
                (0, "b".to_string()),
                (0, "/x/only/solo".to_string()),
            ]
        );
    }

    /// `always`: even a single-project folder gets a header + basename.
    #[test]
    fn always_headers_singletons_too() {
        let entries = vec![entry("solo", "/x/only/solo", None)];
        let model = build_render_model(&entries, ProjectListGrouping::Always);
        assert_eq!(headers(&model), vec!["/x/only".to_string()]);
        assert_eq!(projects(&model), vec![(0, "solo".to_string())]);
    }

    /// `none`: no headers, every row a full path, still path-sorted + nested.
    #[test]
    fn none_is_flat_full_path_no_headers() {
        let entries = vec![
            entry("b", "/x/b", None),
            entry("a", "/x/a", None),
            entry("awt", "/x/a-wt", Some("a")),
        ];
        let model = build_render_model(&entries, ProjectListGrouping::None);
        assert!(headers(&model).is_empty());
        assert_eq!(
            projects(&model),
            vec![
                (0, "/x/a".to_string()),
                (1, "/x/a-wt".to_string()),
                (0, "/x/b".to_string()),
            ]
        );
    }

    /// Child label: basename when the worktree is a sibling or direct child of
    /// its parent; full path when it lives somewhere unrelated.
    #[test]
    fn child_label_basename_vs_full_path() {
        // Sibling in the same directory as its parent → basename.
        let sibling = vec![
            entry("p", "/proj/ceiba/gatekeeper", None),
            entry("wt", "/proj/ceiba/gatekeeper-gaps", Some("p")),
        ];
        let model = build_render_model(&sibling, ProjectListGrouping::Always);
        assert_eq!(
            projects(&model),
            vec![
                (0, "gatekeeper".to_string()),
                (1, "gatekeeper-gaps".to_string()),
            ]
        );

        // Worktree living in a totally different tree → full path.
        let faraway = vec![
            entry("p", "/proj/ceiba/synar", None),
            entry("wt", "/home/downloads/synar", Some("p")),
        ];
        let model = build_render_model(&faraway, ProjectListGrouping::Always);
        assert_eq!(
            projects(&model),
            vec![
                (0, "synar".to_string()),
                (1, "/home/downloads/synar".to_string()),
            ]
        );
    }

    /// The model is a pure function of the set of entries: shuffling the input
    /// order yields an identical render model.
    #[test]
    fn model_is_order_independent() {
        let ordered = vec![
            entry("par", "/proj/parent", None),
            entry("wt", "/proj/parent-wt", Some("par")),
            entry("z", "/proj/zeta", None),
            entry("a", "/other/alpha", None),
        ];
        let shuffled = vec![
            entry("a", "/other/alpha", None),
            entry("wt", "/proj/parent-wt", Some("par")),
            entry("z", "/proj/zeta", None),
            entry("par", "/proj/parent", None),
        ];
        for mode in [
            ProjectListGrouping::Always,
            ProjectListGrouping::Auto,
            ProjectListGrouping::None,
        ] {
            assert_eq!(
                build_render_model(&ordered, mode),
                build_render_model(&shuffled, mode),
                "mode {mode:?} must be order-independent"
            );
        }
    }

    /// Two independent projects in the same folder (neither a sub-project of the
    /// other) both appear as roots under one header — the "different project,
    /// not a worktree" case.
    #[test]
    fn independent_projects_same_folder_both_roots() {
        let entries = vec![
            entry("one", "/shared/one", None),
            entry("two", "/shared/two", None),
        ];
        let model = build_render_model(&entries, ProjectListGrouping::Auto);
        assert_eq!(headers(&model), vec!["/shared".to_string()]);
        assert_eq!(
            projects(&model),
            vec![(0, "one".to_string()), (0, "two".to_string())]
        );
    }
}

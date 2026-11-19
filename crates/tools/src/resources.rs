use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A conservative resource claim calculated before a tool may execute.
///
/// The prepared-call seam records these claims before authority checks and the
/// product scheduler uses them to keep parallel batches non-conflicting.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceClaim {
    ReadOnly,
    WorkspaceRead,
    ReadPath(PathBuf),
    WritePath(PathBuf),
    ReadTree(PathBuf),
    WriteTree(PathBuf),
    Terminal(String),
    ToolCatalogWrite,
    GlobalExclusive,
}

impl ResourceClaim {
    #[must_use]
    pub fn conflicts_with(&self, other: &Self) -> bool {
        use ResourceClaim::{
            GlobalExclusive, ReadOnly, ReadPath, ReadTree, Terminal, ToolCatalogWrite,
            WorkspaceRead, WritePath, WriteTree,
        };

        match (self, other) {
            (GlobalExclusive, _) | (_, GlobalExclusive) => true,
            (ReadOnly, _) | (_, ReadOnly) => false,
            (WorkspaceRead, WritePath(_) | WriteTree(_))
            | (WritePath(_) | WriteTree(_), WorkspaceRead) => true,
            (ToolCatalogWrite, ToolCatalogWrite) => true,
            (Terminal(left), Terminal(right)) => left == right,
            (ReadPath(_), ReadPath(_)) => false,
            (ReadPath(left), WritePath(right)) | (WritePath(right), ReadPath(left)) => {
                left == right
            }
            (WritePath(left), WritePath(right)) => left == right,
            (ReadTree(_), ReadTree(_)) => false,
            (ReadTree(_), ReadPath(_)) | (ReadPath(_), ReadTree(_)) => false,
            (ReadTree(tree), WritePath(path)) | (WritePath(path), ReadTree(tree)) => {
                path.starts_with(tree)
            }
            (WriteTree(tree), ReadPath(path)) | (ReadPath(path), WriteTree(tree)) => {
                path.starts_with(tree)
            }
            (WriteTree(tree), WritePath(path)) | (WritePath(path), WriteTree(tree)) => {
                path.starts_with(tree)
            }
            (ReadTree(left), WriteTree(right)) | (WriteTree(right), ReadTree(left)) => {
                trees_overlap(left, right)
            }
            (WriteTree(left), WriteTree(right)) => trees_overlap(left, right),
            _ => false,
        }
    }
}

fn trees_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

/// Build deterministic parallel batches. Items with no conflicting resource
/// claims share a batch; conflicting items retain their original order.
#[must_use]
pub fn schedule_non_conflicting<T>(items: Vec<(T, Vec<ResourceClaim>)>) -> Vec<Vec<T>> {
    let mut batches = Vec::new();
    let mut batch = Vec::new();
    let mut batch_claims = Vec::new();

    for (item, claims) in items {
        let global_barrier = !batch.is_empty()
            && (claims.contains(&ResourceClaim::GlobalExclusive)
                || batch_claims.contains(&ResourceClaim::GlobalExclusive));
        let conflicts = global_barrier
            || claims.iter().any(|claim| {
                batch_claims
                    .iter()
                    .any(|existing| claim.conflicts_with(existing))
            });
        if conflicts && !batch.is_empty() {
            batches.push(std::mem::take(&mut batch));
            batch_claims.clear();
        }

        batch.push(item);
        batch_claims.extend(claims);
    }

    if !batch.is_empty() {
        batches.push(batch);
    }

    batches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_reads_share_a_batch_but_write_is_ordered() {
        let path = PathBuf::from("src/lib.rs");
        let batches = schedule_non_conflicting(vec![
            ("read-a", vec![ResourceClaim::ReadPath(path.clone())]),
            ("read-b", vec![ResourceClaim::ReadPath(path.clone())]),
            ("write", vec![ResourceClaim::WritePath(path)]),
        ]);
        assert_eq!(batches, vec![vec!["read-a", "read-b"], vec!["write"]]);
    }

    #[test]
    fn unrelated_writes_can_run_together() {
        let batches = schedule_non_conflicting(vec![
            ("a", vec![ResourceClaim::WritePath(PathBuf::from("a.rs"))]),
            ("b", vec![ResourceClaim::WritePath(PathBuf::from("b.rs"))]),
        ]);
        assert_eq!(batches, vec![vec!["a", "b"]]);
    }

    #[test]
    fn intervening_conflict_preserves_contiguous_order() {
        let path = PathBuf::from("src/lib.rs");
        let batches = schedule_non_conflicting(vec![
            ("read-before", vec![ResourceClaim::ReadPath(path.clone())]),
            ("write", vec![ResourceClaim::WritePath(path.clone())]),
            ("read-after", vec![ResourceClaim::ReadPath(path)]),
        ]);
        assert_eq!(
            batches,
            vec![vec!["read-before"], vec!["write"], vec!["read-after"]]
        );
    }

    #[test]
    fn tree_claims_conflict_only_when_a_write_scope_overlaps() {
        let src = PathBuf::from("workspace/src");
        let nested = PathBuf::from("workspace/src/nested");
        let source_file = PathBuf::from("workspace/src/lib.rs");
        let test_file = PathBuf::from("workspace/tests/test.rs");

        assert!(
            !ResourceClaim::ReadTree(src.clone())
                .conflicts_with(&ResourceClaim::ReadPath(source_file.clone()))
        );
        assert!(
            ResourceClaim::ReadTree(src.clone())
                .conflicts_with(&ResourceClaim::WritePath(source_file.clone()))
        );
        assert!(
            !ResourceClaim::ReadTree(src.clone())
                .conflicts_with(&ResourceClaim::WritePath(test_file))
        );
        assert!(
            ResourceClaim::WriteTree(src.clone())
                .conflicts_with(&ResourceClaim::ReadPath(source_file))
        );
        assert!(ResourceClaim::WriteTree(src).conflicts_with(&ResourceClaim::ReadTree(nested)));
    }

    #[test]
    fn global_exclusive_stays_a_singleton_barrier() {
        let batches = schedule_non_conflicting(vec![
            ("before", Vec::new()),
            ("global", vec![ResourceClaim::GlobalExclusive]),
            ("after", Vec::new()),
        ]);
        assert_eq!(batches, vec![vec!["before"], vec!["global"], vec!["after"]]);
    }

    #[test]
    fn global_exclusive_conflicts_with_every_claim() {
        for claim in [
            ResourceClaim::ReadOnly,
            ResourceClaim::WorkspaceRead,
            ResourceClaim::ReadPath(PathBuf::from("src/lib.rs")),
            ResourceClaim::Terminal("shell-1".to_string()),
            ResourceClaim::ToolCatalogWrite,
            ResourceClaim::GlobalExclusive,
        ] {
            assert!(ResourceClaim::GlobalExclusive.conflicts_with(&claim));
            assert!(claim.conflicts_with(&ResourceClaim::GlobalExclusive));
        }
    }

    #[test]
    fn readonly_workspace_read_and_catalog_claims_schedule_with_write_barriers() {
        let batches = schedule_non_conflicting(vec![
            ("read", vec![ResourceClaim::ReadPath(PathBuf::from("a.rs"))]),
            ("git_status", vec![ResourceClaim::WorkspaceRead]),
            ("bash_ro", vec![ResourceClaim::WorkspaceRead]),
            ("load_skill", vec![ResourceClaim::ReadOnly]),
            ("tool_search_1", vec![ResourceClaim::ToolCatalogWrite]),
            ("grep", vec![ResourceClaim::ReadTree(PathBuf::from("src"))]),
            ("tool_search_2", vec![ResourceClaim::ToolCatalogWrite]),
            (
                "edit",
                vec![ResourceClaim::WritePath(PathBuf::from("b.rs"))],
            ),
            ("git_diff", vec![ResourceClaim::WorkspaceRead]),
            ("mutating_bash", vec![ResourceClaim::GlobalExclusive]),
            ("session_search", vec![ResourceClaim::ReadOnly]),
        ]);
        assert_eq!(
            batches,
            vec![
                vec![
                    "read",
                    "git_status",
                    "bash_ro",
                    "load_skill",
                    "tool_search_1",
                    "grep"
                ],
                vec!["tool_search_2", "edit"],
                vec!["git_diff"],
                vec!["mutating_bash"],
                vec!["session_search"],
            ]
        );
    }

    #[test]
    fn workspace_read_conflicts_with_writes_but_not_reads() {
        assert!(
            !ResourceClaim::WorkspaceRead
                .conflicts_with(&ResourceClaim::ReadPath(PathBuf::from("a.rs")))
        );
        assert!(
            !ResourceClaim::WorkspaceRead
                .conflicts_with(&ResourceClaim::ReadTree(PathBuf::from("src")))
        );
        assert!(!ResourceClaim::WorkspaceRead.conflicts_with(&ResourceClaim::ReadOnly));
        assert!(!ResourceClaim::WorkspaceRead.conflicts_with(&ResourceClaim::WorkspaceRead));
        assert!(!ResourceClaim::WorkspaceRead.conflicts_with(&ResourceClaim::ToolCatalogWrite));
        assert!(
            ResourceClaim::WorkspaceRead
                .conflicts_with(&ResourceClaim::WritePath(PathBuf::from("a.rs")))
        );
        assert!(
            ResourceClaim::WritePath(PathBuf::from("a.rs"))
                .conflicts_with(&ResourceClaim::WorkspaceRead)
        );
        assert!(
            ResourceClaim::WorkspaceRead
                .conflicts_with(&ResourceClaim::WriteTree(PathBuf::from("src")))
        );
        assert!(
            ResourceClaim::WriteTree(PathBuf::from("src"))
                .conflicts_with(&ResourceClaim::WorkspaceRead)
        );
    }
}

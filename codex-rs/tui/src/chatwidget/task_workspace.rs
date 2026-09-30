//! Task-local file accounting without workspace-wide filesystem scans.

use std::path::Path;

use codex_app_server_protocol::FileUpdateChange;
use codex_app_server_protocol::PatchChangeKind;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct WorkspaceDiffStats {
    pub(super) files_changed: usize,
    pub(super) files_created: usize,
    pub(super) files_deleted: usize,
    pub(super) lines_added: usize,
    pub(super) lines_removed: usize,
}

impl WorkspaceDiffStats {
    pub(super) fn is_empty(self) -> bool {
        self == Self::default()
    }

    pub(super) fn add_assign(&mut self, other: Self) {
        self.files_changed = self.files_changed.saturating_add(other.files_changed);
        self.files_created = self.files_created.saturating_add(other.files_created);
        self.files_deleted = self.files_deleted.saturating_add(other.files_deleted);
        self.lines_added = self.lines_added.saturating_add(other.lines_added);
        self.lines_removed = self.lines_removed.saturating_add(other.lines_removed);
    }

    pub(super) fn files_modified(self) -> usize {
        self.files_changed
            .saturating_sub(self.files_created.saturating_add(self.files_deleted))
    }
}

#[derive(Debug)]
pub(super) struct TaskWorkspaceTracker {
    direct_stats: WorkspaceDiffStats,
}

impl TaskWorkspaceTracker {
    pub(super) fn start(_task_id: Option<&str>) -> Self {
        Self {
            direct_stats: WorkspaceDiffStats::default(),
        }
    }

    /// Records the server's authoritative patch diff. This is intentionally
    /// independent of local command execution because apply-patch runs inside
    /// Codex and emits the authoritative change payload directly.
    pub(super) fn record_file_changes(&mut self, changes: &[FileUpdateChange], _cwd: &Path) {
        for change in changes {
            self.direct_stats
                .add_assign(file_update_change_stats(change));
        }
    }

    pub(super) fn finish(self) -> WorkspaceDiffStats {
        self.direct_stats
    }
}

fn file_update_change_stats(change: &FileUpdateChange) -> WorkspaceDiffStats {
    let mut stats = WorkspaceDiffStats {
        files_changed: 1,
        ..WorkspaceDiffStats::default()
    };
    match &change.kind {
        PatchChangeKind::Add => {
            stats.files_created = 1;
            stats.lines_added = count_lines(change.diff.as_bytes());
        }
        PatchChangeKind::Delete => {
            stats.files_deleted = 1;
            stats.lines_removed = count_lines(change.diff.as_bytes());
        }
        PatchChangeKind::Update { move_path } => {
            let (lines_added, lines_removed) = count_unified_diff_lines(&change.diff);
            stats.lines_added = lines_added;
            stats.lines_removed = lines_removed;
            if move_path.is_some() {
                stats.files_changed = 2;
                stats.files_deleted = 1;
                stats.files_created = 1;
            }
        }
    }
    stats
}

fn count_lines(content: &[u8]) -> usize {
    if content.is_empty() {
        0
    } else {
        content.iter().filter(|byte| **byte == b'\n').count()
            + usize::from(!content.ends_with(b"\n"))
    }
}

fn count_unified_diff_lines(diff: &str) -> (usize, usize) {
    diff.lines().fold((0, 0), |(added, removed), line| {
        if line.starts_with("+++") || line.starts_with("---") {
            (added, removed)
        } else if line.starts_with('+') {
            (added + 1, removed)
        } else if line.starts_with('-') {
            (added, removed + 1)
        } else {
            (added, removed)
        }
    })
}

#[cfg(test)]
#[path = "task_workspace_tests.rs"]
mod tests;

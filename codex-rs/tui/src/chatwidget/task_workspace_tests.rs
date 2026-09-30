use super::*;
use codex_app_server_protocol::FileUpdateChange;
use codex_app_server_protocol::PatchChangeKind;
use pretty_assertions::assert_eq;

#[test]
fn direct_add_and_update_diffs_are_counted_from_server_payloads() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let mut tracker = TaskWorkspaceTracker::start(None);
    tracker.record_file_changes(
        &[
            FileUpdateChange {
                path: "new.txt".to_string(),
                kind: PatchChangeKind::Add,
                diff: "one\ntwo\n".to_string(),
            },
            FileUpdateChange {
                path: "old.txt".to_string(),
                kind: PatchChangeKind::Update { move_path: None },
                diff: "@@ -1 +1,2 @@\n-old\n+new\n+line\n".to_string(),
            },
        ],
        directory.path(),
    );

    assert_eq!(
        tracker.finish(),
        WorkspaceDiffStats {
            files_changed: 2,
            files_created: 1,
            files_deleted: 0,
            lines_added: 4,
            lines_removed: 1,
        }
    );
}

//! A stable rollout can keep its parsed history across resume's configuration reload.

use codex_rollout::InitialHistory;
use codex_thread_store::StoredThread;
use std::path::Path;
use std::time::SystemTime;

pub(super) struct PreparedResumeHistory {
    pub history: InitialHistory,
    pub source: StoredThread,
    pub fingerprint: RolloutFingerprint,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RolloutFingerprint {
    len: u64,
    modified: SystemTime,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
}

impl RolloutFingerprint {
    pub async fn read(path: &Path) -> Option<Self> {
        let metadata = tokio::fs::metadata(path).await.ok()?;
        if !metadata.is_file() {
            return None;
        }
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        Some(Self {
            len: metadata.len(),
            modified: metadata.modified().ok()?,
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            changed_seconds: metadata.ctime(),
            #[cfg(unix)]
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }
}

#[cfg(test)]
#[path = "resume_history_cache_tests.rs"]
mod tests;

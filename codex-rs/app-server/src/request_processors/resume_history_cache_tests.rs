use super::RolloutFingerprint;
use pretty_assertions::assert_ne;
use std::fs;

#[tokio::test]
async fn appended_rollout_invalidates_resume_cache() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("rollout.jsonl");
    fs::write(&path, b"first\n").unwrap();
    let original = RolloutFingerprint::read(&path).await.unwrap();

    fs::write(&path, b"first\nsecond\n").unwrap();
    let appended = RolloutFingerprint::read(&path).await.unwrap();
    assert_ne!(original, appended);
}

#[cfg(unix)]
#[tokio::test]
async fn replaced_rollout_invalidates_resume_cache() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("rollout.jsonl");
    fs::write(&path, b"first\nsecond\n").unwrap();
    let original = RolloutFingerprint::read(&path).await.unwrap();
    let replacement = temp.path().join("replacement.jsonl");
    fs::write(&replacement, b"first\nsecond\n").unwrap();
    fs::rename(&replacement, &path).unwrap();
    let replaced = RolloutFingerprint::read(&path).await.unwrap();
    assert_ne!(original, replaced);
}

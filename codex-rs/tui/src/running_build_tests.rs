//! Build-ID fixtures are tiny, valid ELF files, never executable test programs.

use super::*;
use launcher::Cache;
use launcher::Launcher;
use pretty_assertions::assert_eq;
use std::fs;
use std::fs::File;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use std::path::Path;

fn write_elf(path: &Path, id: u8) {
    let mut bytes = vec![0_u8; 140];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16..18].copy_from_slice(&3_u16.to_le_bytes());
    bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
    bytes[32..40].copy_from_slice(&64_u64.to_le_bytes());
    bytes[52..54].copy_from_slice(&64_u16.to_le_bytes());
    bytes[54..56].copy_from_slice(&56_u16.to_le_bytes());
    bytes[56..58].copy_from_slice(&1_u16.to_le_bytes());
    bytes[64..68].copy_from_slice(&4_u32.to_le_bytes());
    bytes[72..80].copy_from_slice(&120_u64.to_le_bytes());
    bytes[96..104].copy_from_slice(&20_u64.to_le_bytes());
    bytes[120..124].copy_from_slice(&4_u32.to_le_bytes());
    bytes[124..128].copy_from_slice(&1_u32.to_le_bytes());
    bytes[128..132].copy_from_slice(&3_u32.to_le_bytes());
    bytes[132..136].copy_from_slice(b"GNU\0");
    bytes[136] = id;
    let written = fs::write(path, bytes);
    assert!(written.is_ok(), "write tiny ELF fixture: {written:?}");
    let permissions = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
    assert!(
        permissions.is_ok(),
        "make ELF fixture executable: {permissions:?}"
    );
}

#[test]
fn build_identity_tracks_replacement_retargeting_restoration_and_unknown_candidates() {
    let root = tempfile::tempdir().unwrap();
    let a = root.path().join("a");
    let b = root.path().join("b");
    let link = root.path().join("codex");
    write_elf(&a, /*id*/ 1);
    write_elf(&b, /*id*/ 2);
    symlink(&a, &link).unwrap();
    let launcher = Launcher {
        path: link.clone(),
        native_target: None,
        cwd: root.path().into(),
        search_path: vec![],
    };
    let mut cache = Cache::default();
    assert_eq!(cache.check(&launcher).0, Some(vec![1]));
    assert_eq!(cache.check(&launcher).0, Some(vec![1]));
    fs::rename(&b, &a).unwrap();
    assert_eq!(cache.check(&launcher).0, Some(vec![2]));
    write_elf(&b, /*id*/ 1);
    fs::remove_file(&link).unwrap();
    symlink(&b, &link).unwrap();
    assert_eq!(cache.check(&launcher).0, Some(vec![1]));
    fs::write(&b, b"\x7fELF incomplete").unwrap();
    assert_eq!(cache.check(&launcher).0, None);
    fs::remove_file(&b).unwrap();
    assert_eq!(cache.check(&launcher).0, None);
    write_elf(&b, /*id*/ 2);
    fs::set_permissions(&b, fs::Permissions::from_mode(0o000)).unwrap();
    assert_eq!(cache.check(&launcher).0, None);
}

#[test]
fn static_wrappers_and_explicit_native_targets_are_read_without_execution() {
    let root = tempfile::tempdir().unwrap();
    let native = root.path().join("native codex");
    let wrapper = root.path().join("wrapper");
    let sentinel = root.path().join("must-not-exist");
    write_elf(&native, /*id*/ 7);
    fs::write(
        &wrapper,
        format!("#!/bin/sh\nexec '{}' \"$@\"\n", native.display()),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let mut launcher = Launcher {
        path: wrapper.clone(),
        native_target: None,
        cwd: root.path().into(),
        search_path: vec![],
    };
    let mut cache = Cache::default();
    assert_eq!(cache.check(&launcher).0, Some(vec![7]));
    fs::write(&wrapper, "#!/bin/sh\nexec native* \"$@\"\n").unwrap();
    assert_eq!(cache.check(&launcher).0, None);
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\ntouch '{}'\nexec '{}' \"$@\"\n",
            sentinel.display(),
            native.display()
        ),
    )
    .unwrap();
    assert_eq!(cache.check(&launcher).0, None);
    assert!(!sentinel.exists());
    launcher.native_target = Some(native);
    assert_eq!(cache.check(&launcher).0, Some(vec![7]));
    assert!(!sentinel.exists());
}

#[test]
fn directory_symlinks_and_bare_launcher_path_precedence_are_rechecked() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    let directory = root.path().join("selected");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    write_elf(&second.join("codex"), /*id*/ 2);
    symlink("second", &directory).unwrap();
    let mut launcher = Launcher {
        path: directory.join("codex"),
        native_target: None,
        cwd: root.path().into(),
        search_path: vec![first.clone(), second],
    };
    let mut cache = Cache::default();
    assert_eq!(cache.check(&launcher).0, Some(vec![2]));
    write_elf(&first.join("codex"), /*id*/ 1);
    fs::remove_file(&directory).unwrap();
    symlink("first", &directory).unwrap();
    assert_eq!(cache.check(&launcher).0, Some(vec![1]));
    launcher.path = "codex".into();
    assert_eq!(cache.check(&launcher).0, Some(vec![1]));
    fs::remove_file(first.join("codex")).unwrap();
    assert_eq!(cache.check(&launcher).0, Some(vec![2]));
}

#[test]
fn malformed_or_truncated_elf_notes_are_unknown() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("codex");
    write_elf(&path, /*id*/ 4);
    let bytes = fs::read(&path).unwrap();
    for length in 0..bytes.len() {
        fs::write(&path, &bytes[..length]).unwrap();
        assert_eq!(
            elf::read_build_id(&File::open(&path).unwrap()).unwrap(),
            None
        );
    }
    let mut bad = bytes.clone();
    bad[120..124].copy_from_slice(&u32::MAX.to_le_bytes());
    fs::write(&path, bad).unwrap();
    assert_eq!(
        elf::read_build_id(&File::open(&path).unwrap()).unwrap(),
        None
    );
    fs::write(&path, bytes).unwrap();
    assert_eq!(
        elf::read_build_id(&File::open(&path).unwrap()).unwrap(),
        Some(vec![4])
    );
}

#[test]
fn running_identity_is_read_from_the_native_proc_inode() {
    let file = File::open("/proc/self/exe").unwrap();
    assert!(elf::read_build_id(&file).unwrap().is_some());
}

#[tokio::test]
async fn monitor_debounces_retargets_retains_unknown_and_clears_on_restoration() {
    use std::time::Duration;
    use tokio::time::timeout;

    let root = tempfile::tempdir().unwrap();
    let a = root.path().join("a");
    let b = root.path().join("b");
    let link = root.path().join("codex");
    write_elf(&a, /*id*/ 1);
    write_elf(&b, /*id*/ 2);
    symlink(&a, &link).unwrap();
    let launcher = Launcher {
        path: link.clone(),
        native_target: None,
        cwd: root.path().into(),
        search_path: vec![],
    };
    let (tx, mut rx) = tokio::sync::watch::channel(/*init*/ false);
    let task = tokio::spawn(monitor::run(
        tx,
        launcher,
        vec![1],
        Duration::from_secs(/*secs*/ 1),
        Duration::from_secs(/*secs*/ 30),
    ));
    tokio::time::sleep(Duration::from_millis(/*millis*/ 150)).await;
    // An unrelated write must not trigger a title update.
    fs::write(root.path().join("unrelated"), b"noise").unwrap();
    fs::remove_file(&link).unwrap();
    symlink(&b, &link).unwrap();
    assert!(
        timeout(Duration::from_millis(/*millis*/ 700), rx.changed())
            .await
            .is_err()
    );
    timeout(Duration::from_secs(/*secs*/ 3), rx.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(*rx.borrow());
    fs::write(&b, b"incomplete linker output").unwrap();
    tokio::time::sleep(Duration::from_millis(/*millis*/ 1200)).await;
    assert!(*rx.borrow());
    assert!(!rx.has_changed().unwrap());
    fs::remove_file(&link).unwrap();
    symlink(&a, &link).unwrap();
    timeout(Duration::from_secs(/*secs*/ 3), rx.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(!*rx.borrow());
    // An atomic native-binary replacement is detected even when the symlink is unchanged.
    write_elf(&b, /*id*/ 2);
    fs::rename(&b, &a).unwrap();
    timeout(Duration::from_secs(/*secs*/ 3), rx.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(*rx.borrow());
    write_elf(&b, /*id*/ 1);
    fs::rename(&b, &a).unwrap();
    timeout(Duration::from_secs(/*secs*/ 3), rx.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(!*rx.borrow());
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn periodic_check_covers_missing_watch_directories() {
    use std::time::Duration;

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("not-created-yet/codex");
    let launcher = Launcher {
        path: path.clone(),
        native_target: None,
        cwd: root.path().into(),
        search_path: vec![],
    };
    let (tx, mut rx) = tokio::sync::watch::channel(/*init*/ false);
    let task = tokio::spawn(monitor::run(
        tx,
        launcher,
        vec![1],
        Duration::from_secs(/*secs*/ 10),
        Duration::from_millis(/*millis*/ 100),
    ));
    tokio::time::sleep(Duration::from_millis(/*millis*/ 150)).await;
    fs::create_dir(path.parent().unwrap()).unwrap();
    write_elf(&path, /*id*/ 2);
    tokio::time::timeout(Duration::from_secs(/*secs*/ 3), rx.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(*rx.borrow());
    task.abort();
    let _ = task.await;
}

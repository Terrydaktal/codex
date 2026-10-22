//! Preserve launch indirection and interpret only static shell exec wrappers.

use super::elf::read_build_id;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs;
use std::fs::Metadata;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;

pub(super) struct Launcher {
    pub path: PathBuf,
    pub native_target: Option<PathBuf>,
    pub cwd: PathBuf,
    pub search_path: Vec<PathBuf>,
}

impl Launcher {
    pub fn capture() -> Option<Self> {
        Some(Self {
            path: std::env::var_os("CODEX_LAUNCHER_PATH")
                .or_else(|| std::env::args_os().next())?
                .into(),
            native_target: std::env::var_os("CODEX_LAUNCHER_EXECUTABLE").map(PathBuf::from),
            cwd: std::env::current_dir().ok()?,
            search_path: std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .take(128)
                .collect(),
        })
    }

    fn locate(&self, program: &Path, paths: &mut BTreeSet<PathBuf>) -> Option<PathBuf> {
        if program.is_absolute() || program.components().count() > 1 {
            return Some(self.cwd.join(program));
        }
        for directory in &self.search_path {
            let candidate = self.cwd.join(directory).join(program);
            paths.insert(candidate.clone());
            if fs::metadata(&candidate).is_ok_and(|meta| meta.is_file() && meta.mode() & 0o111 != 0)
            {
                return Some(candidate);
            }
        }
        None
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    length: u64,
    mode: u32,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl From<&Metadata> for Stamp {
    fn from(meta: &Metadata) -> Self {
        Self {
            device: meta.dev(),
            inode: meta.ino(),
            length: meta.len(),
            mode: meta.mode(),
            modified: (meta.mtime(), meta.mtime_nsec()),
            changed: (meta.ctime(), meta.ctime_nsec()),
        }
    }
}

#[derive(Clone)]
enum Kind {
    Native(Vec<u8>),
    Wrapper(Option<PathBuf>),
}

#[derive(Default)]
pub(super) struct Cache(BTreeMap<PathBuf, (Stamp, Kind)>);

impl Cache {
    pub fn check(&mut self, launcher: &Launcher) -> (Option<Vec<u8>>, BTreeSet<PathBuf>) {
        let mut paths = BTreeSet::new();
        let mut next_cache = BTreeMap::new();
        let id = (|| {
            let mut candidate = launcher.locate(&launcher.path, &mut paths)?;
            for _ in 0..8 {
                let path = follow_links(&candidate, &mut paths)?;
                let file = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&path)
                    .ok()?;
                let metadata = file.metadata().ok()?;
                if !metadata.is_file() || metadata.mode() & 0o111 == 0 {
                    return None;
                }
                let stamp = Stamp::from(&metadata);
                let kind =
                    if let Some((_, kind)) = self.0.get(&path).filter(|(old, _)| old == &stamp) {
                        kind.clone()
                    } else if let Some(id) = read_build_id(&file).ok()? {
                        Kind::Native(id)
                    } else {
                        let mut bytes = Vec::new();
                        file.take(16385).read_to_end(&mut bytes).ok()?;
                        if bytes.len() > 16384 || !bytes.starts_with(b"#!") {
                            return None;
                        }
                        Kind::Wrapper(static_wrapper_target(&bytes))
                    };
                if fs::metadata(&path).ok().map(|meta| Stamp::from(&meta)) != Some(stamp.clone()) {
                    return None;
                }
                next_cache.insert(path, (stamp, kind.clone()));
                match kind {
                    Kind::Native(id) => return Some(id),
                    Kind::Wrapper(target) => {
                        let target = target.or_else(|| launcher.native_target.clone())?;
                        candidate = launcher.locate(&target, &mut paths)?;
                    }
                }
            }
            None
        })();
        self.0 = next_cache;
        (id, paths)
    }
}

fn static_wrapper_target(bytes: &[u8]) -> Option<PathBuf> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.lines();
    if !matches!(
        lines.next()?,
        "#!/bin/sh"
            | "#!/bin/bash"
            | "#!/usr/bin/bash"
            | "#!/usr/bin/env bash"
            | "#!/usr/bin/env sh"
    ) {
        return None;
    }
    let mut commands = lines
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    let words = shlex::split(commands.next()?)?;
    if commands.next().is_some()
        || !matches!(words.len(), 2 | 3)
        || words[0] != "exec"
        || (words.len() == 3 && words[2] != "$@")
        || words[1].is_empty()
        || words[1].contains([
            '$', '`', ';', '|', '&', '<', '>', '*', '?', '[', ']', '{', '}', '(', ')', '~',
        ])
    {
        return None;
    }
    Some(PathBuf::from(&words[1]))
}

fn follow_links(path: &Path, paths: &mut BTreeSet<PathBuf>) -> Option<PathBuf> {
    let mut parts: VecDeque<OsString> = path
        .components()
        .map(|part| part.as_os_str().to_owned())
        .collect();
    let mut resolved = PathBuf::from("/");
    let mut hops = 0;
    while let Some(part) = parts.pop_front() {
        match part.to_str() {
            Some("/") => {
                resolved = PathBuf::from("/");
                continue;
            }
            Some(".") => continue,
            Some("..") => {
                resolved.pop();
                continue;
            }
            _ => {}
        }
        let entry = resolved.join(part);
        paths.insert(entry.clone());
        if fs::symlink_metadata(&entry).ok()?.file_type().is_symlink() {
            hops += 1;
            if hops > 40 {
                return None;
            }
            let target = fs::read_link(entry).ok()?;
            for part in target.components().rev() {
                parts.push_front(part.as_os_str().to_owned());
            }
        } else {
            resolved = entry;
        }
    }
    Some(resolved)
}

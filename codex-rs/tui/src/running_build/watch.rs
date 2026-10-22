//! Nonrecursive inotify watches, filtered to launch indirections and the native target.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::ffi::CString;
use std::ffi::OsString;
use std::io;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use tokio::io::unix::AsyncFd;

pub(super) struct Watch {
    fd: AsyncFd<OwnedFd>,
    names: BTreeMap<i32, BTreeSet<OsString>>,
}

impl Watch {
    pub fn new(paths: &BTreeSet<PathBuf>) -> io::Result<Self> {
        // SAFETY: inotify returns a new owned descriptor, or -1 without transferring ownership.
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd was just created and is owned only by this guard.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut names: BTreeMap<i32, BTreeSet<OsString>> = BTreeMap::new();
        let mask = libc::IN_CREATE
            | libc::IN_DELETE
            | libc::IN_MOVED_FROM
            | libc::IN_MOVED_TO
            | libc::IN_MODIFY
            | libc::IN_ATTRIB
            | libc::IN_CLOSE_WRITE
            | libc::IN_DELETE_SELF
            | libc::IN_MOVE_SELF;
        for path in paths {
            let Some((parent, name)) = path.parent().zip(path.file_name()) else {
                continue;
            };
            let Ok(parent) = CString::new(parent.as_os_str().as_bytes()) else {
                continue;
            };
            // SAFETY: parent is a valid NUL-terminated string; fd is live for this call.
            let wd = unsafe { libc::inotify_add_watch(fd.as_raw_fd(), parent.as_ptr(), mask) };
            if wd >= 0 {
                names.entry(wd).or_default().insert(name.to_owned());
            }
        }
        if names.is_empty() {
            return Err(io::Error::other("no readable launcher directories"));
        }
        Ok(Self {
            fd: AsyncFd::new(fd)?,
            names,
        })
    }

    pub async fn changed(&mut self) -> io::Result<()> {
        loop {
            let mut ready = self.fd.readable().await?;
            let result = ready.try_io(|fd| {
                let mut bytes = [0_u8; 8192];
                // SAFETY: bytes is writable for the supplied length; the descriptor is live.
                let length =
                    unsafe { libc::read(fd.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len()) };
                if length < 0 {
                    return Err(io::Error::last_os_error());
                }
                if length == 0 {
                    return Err(io::Error::other("inotify closed"));
                }
                let bytes = &bytes[..length as usize];
                let mut pos = 0;
                let mut relevant = false;
                while pos + std::mem::size_of::<libc::inotify_event>() <= bytes.len() {
                    // SAFETY: the complete header fits; read_unaligned avoids buffer alignment assumptions.
                    let event = unsafe {
                        bytes
                            .as_ptr()
                            .add(pos)
                            .cast::<libc::inotify_event>()
                            .read_unaligned()
                    };
                    pos += std::mem::size_of::<libc::inotify_event>();
                    let Some(name) = bytes.get(pos..pos + event.len as usize) else {
                        break;
                    };
                    let end = name
                        .iter()
                        .position(|byte| *byte == 0)
                        .unwrap_or(name.len());
                    let name = OsString::from_vec(name[..end].to_vec());
                    relevant |= event.mask
                        & (libc::IN_Q_OVERFLOW
                            | libc::IN_IGNORED
                            | libc::IN_DELETE_SELF
                            | libc::IN_MOVE_SELF)
                        != 0
                        || self
                            .names
                            .get(&event.wd)
                            .is_some_and(|names| names.contains(&name));
                    pos += event.len as usize;
                }
                Ok(relevant)
            });
            match result {
                Ok(Ok(true)) => return Ok(()),
                Ok(Ok(false)) => tokio::task::yield_now().await,
                Err(_) => {}
                Ok(Err(error)) => return Err(error),
            }
        }
    }
}

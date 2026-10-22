use super::elf::read_build_id;
use super::launcher::Cache;
use super::launcher::Launcher;
use super::watch::Watch;
use std::collections::BTreeSet;
use std::fs::File;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::Instant;

const DEBOUNCE: Duration = Duration::from_secs(/*secs*/ 1);
const FALLBACK: Duration = Duration::from_secs(/*secs*/ 30);

pub(crate) struct Monitor(JoinHandle<()>);

impl Drop for Monitor {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(crate) fn start(tx: tokio::sync::watch::Sender<bool>) -> Option<Monitor> {
    let launcher = Launcher::capture()?;
    Some(Monitor(tokio::spawn(async move {
        // /proc/self/exe retains the running inode even after unlink/replacement of the launcher.
        let Ok(Some(running_id)) = tokio::task::spawn_blocking(|| {
            File::open("/proc/self/exe")
                .ok()
                .and_then(|file| read_build_id(&file).ok().flatten())
        })
        .await
        else {
            return;
        };
        run(tx, launcher, running_id, DEBOUNCE, FALLBACK).await;
    })))
}

pub(super) async fn run(
    tx: tokio::sync::watch::Sender<bool>,
    mut launcher: Launcher,
    running_id: Vec<u8>,
    debounce: Duration,
    fallback: Duration,
) {
    let mut cache = Cache::default();
    let mut watched = BTreeSet::new();
    let mut watcher: Option<Watch> = None;
    let mut due = Instant::now();
    let mut periodic = Instant::now() + fallback;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(due.min(periodic)) => {
                let checked = tokio::task::spawn_blocking(move || {
                    let checked = cache.check(&launcher);
                    (launcher, cache, checked)
                }).await;
                let Ok((next_launcher, next_cache, (candidate, paths))) = checked else { return; };
                launcher = next_launcher;
                cache = next_cache;
                let now = Instant::now();
                due = now + fallback;
                periodic = now + fallback;
                if watcher.is_none() || watched != paths {
                    watcher = Watch::new(&paths).ok();
                    watched = paths;
                    // Close the read-before-watch race for a new or retargeted launcher.
                    if watcher.is_some() { due = now; }
                }
                if let Some(id) = candidate {
                    let outdated = id != running_id;
                    tx.send_if_modified(|previous| {
                        if *previous == outdated { return false; }
                        *previous = outdated;
                        true
                    });
                } else {
                    // Unknown must not clear a previously established stale marker.
                    tracing::debug!("running build candidate identity is unknown");
                }
            }
            changed = async {
                if let Some(watcher) = watcher.as_mut() { watcher.changed().await }
                else { std::future::pending().await }
            } => {
                if changed.is_err() { watcher = None; }
                due = Instant::now() + debounce;
            }
        }
    }
}

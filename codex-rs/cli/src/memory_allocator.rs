//! Bound glibc's retention of freed history, image, and response buffers.
//!
//! Its adaptive mmap threshold can move multi-megabyte allocations into arenas,
//! where tiny live allocations pin otherwise empty heaps indefinitely. Keep large
//! buffers separately mapped and reclaim free arena pages on a coarse worker,
//! never in input, rendering, or response handling. This process-local policy is
//! not exported to child tools and does not change other platforms' allocators.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const MMAP_THRESHOLD_BYTES: libc::c_int = 128 * 1024;
const RECLAIM_INTERVAL: Duration = Duration::from_secs(/*secs*/ 60);

pub(super) struct HeapReclaimer {
    stop: mpsc::Sender<()>,
    worker: Option<thread::JoinHandle<()>>,
}

pub(super) fn start() -> Option<HeapReclaimer> {
    start_with_interval(RECLAIM_INTERVAL)
}

/// Configure only at the beginning of main, before dispatch creates any threads.
pub(super) fn configure_before_threads() {
    // SAFETY: The CLI calls this before build-info initialization, arg0 dispatch
    // and the runtime. glibc marks mallopt MT-Unsafe; never tune it on a worker.
    // Setting a fixed threshold disables adaptation without changing child env.
    unsafe { libc::mallopt(libc::M_MMAP_THRESHOLD, MMAP_THRESHOLD_BYTES) };
}

fn start_with_interval(interval: Duration) -> Option<HeapReclaimer> {
    let (stop, receiver) = mpsc::channel();
    let worker = thread::Builder::new()
        .name("codex-heap-reclaim".into())
        .spawn(move || {
            while let Err(mpsc::RecvTimeoutError::Timeout) = receiver.recv_timeout(interval) {
                // SAFETY: malloc_trim is thread-safe and releases only free pages.
                // glibc keeps live allocations and their addresses unchanged.
                unsafe {
                    libc::malloc_trim(/*__pad*/ 0)
                };
            }
        })
        .ok()?;
    Some(HeapReclaimer {
        stop,
        worker: Some(worker),
    })
}

impl Drop for HeapReclaimer {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
#[path = "memory_allocator_tests.rs"]
mod tests;

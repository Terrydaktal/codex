use super::*;
use std::process::Command;
use std::time::Instant;

const PROBE_ENV: &str = "CODEX_TEST_ALLOCATOR_PROBE";
const MIB: usize = 1024 * 1024;

#[test]
fn large_freed_buffers_do_not_pin_the_arena() {
    let retained = probe("unrestricted");
    let bounded = probe("bounded");
    println!("default retained {retained} KiB; bounded retained {bounded} KiB");
    assert!(
        retained > 128 * 1024,
        "control retained only {retained} KiB"
    );
    assert!(bounded < 16 * 1024, "policy retained {bounded} KiB");
}

#[test]
fn background_worker_reclaims_small_freed_buffers_and_stops_promptly() {
    assert!(probe("reclaim") < 16 * 1024);
}

fn probe(mode: &str) -> usize {
    // Fresh processes isolate mallopt, the synthetic allocations, and inherited
    // user allocator settings from the parallel test runner and real sessions.
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "memory_allocator::tests::allocator_probe",
            "--nocapture",
        ])
        .env(PROBE_ENV, mode)
        .env_remove("GLIBC_TUNABLES")
        .env_remove("LD_PRELOAD")
        .env_remove("MALLOC_MMAP_THRESHOLD_")
        .env_remove("MALLOC_ARENA_MAX")
        .env_remove("MALLOC_TRIM_THRESHOLD_")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("retained_kib="))
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn allocator_probe() {
    let Ok(mode) = std::env::var(PROBE_ENV) else {
        return;
    };
    // This exact-test child has no concurrent allocator workload. Production
    // ordering before any threads is covered by the real CLI/preload regression.
    if mode != "unrestricted" {
        configure_before_threads();
    }
    let reclaimer = match mode.as_str() {
        "unrestricted" => None,
        "bounded" => start(),
        "reclaim" => start_with_interval(Duration::from_millis(/*millis*/ 10)),
        _ => panic!("unknown allocator probe"),
    };
    if mode != "unrestricted" {
        assert!(reclaimer.is_some());
    }

    // This free raises the default adaptive mmap threshold, reproducing a large
    // history load followed by repeated smaller image/response allocations.
    let warmup = allocate(16 * MIB);
    unsafe { libc::free(warmup) };
    let baseline = resident_kib();
    let (count, size) = if mode == "reclaim" {
        (1024, 64 * 1024)
    } else {
        (128, 2 * MIB)
    };
    let mut buffers = Vec::new();
    let mut pins = Vec::new();
    for _ in 0..count {
        buffers.push(allocate(size));
        pins.push(allocate(/*bytes*/ 128));
    }
    assert!(resident_kib().saturating_sub(baseline) > 32 * 1024);
    for buffer in buffers {
        unsafe { libc::free(buffer) };
    }

    if mode == "reclaim" {
        let deadline = Instant::now() + Duration::from_secs(/*secs*/ 3);
        while resident_kib().saturating_sub(baseline) >= 16 * 1024 {
            assert!(
                Instant::now() < deadline,
                "free arena pages were not reclaimed"
            );
            thread::sleep(Duration::from_millis(/*millis*/ 10));
        }
    }
    let retained = resident_kib().saturating_sub(baseline);
    let stopped = Instant::now();
    drop(reclaimer);
    assert!(stopped.elapsed() < Duration::from_secs(/*secs*/ 1));
    for pin in pins {
        unsafe { libc::free(pin) };
    }
    println!("retained_kib={retained}");
}

fn allocate(bytes: usize) -> *mut libc::c_void {
    // SAFETY: Each allocation is checked, touched within bounds, and freed once
    // by the synthetic probe. Touching pages makes RSS, not virtual size, testable.
    unsafe {
        let pointer = libc::malloc(bytes);
        assert!(!pointer.is_null());
        std::ptr::write_bytes(pointer.cast::<u8>(), /*val*/ 1, bytes);
        pointer
    }
}

fn resident_kib() -> usize {
    std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

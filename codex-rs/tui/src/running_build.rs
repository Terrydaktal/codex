//! Local, Linux-only build identity monitoring; unrelated to server or model versions.
//! Filesystem work stays off the foreground loop. Title rendering only reads a shared flag.

#[cfg(target_os = "linux")]
mod elf;
#[cfg(target_os = "linux")]
mod launcher;
#[cfg(target_os = "linux")]
mod monitor;
#[cfg(target_os = "linux")]
mod watch;

#[cfg(target_os = "linux")]
pub(crate) use monitor::start;

pub(crate) const OUTDATED_LABEL: &str = "c\u{0336}o\u{0336}d\u{0336}e\u{0336}x\u{0336}";

pub(crate) fn app_label(outdated: bool) -> &'static str {
    if outdated { OUTDATED_LABEL } else { "codex" }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "running_build_tests.rs"]
mod tests;

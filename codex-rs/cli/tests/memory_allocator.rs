//! Check allocator initialization ordering in the real CLI, not the test harness.
#![cfg(all(target_os = "linux", target_env = "gnu"))]

use anyhow::Result;
use anyhow::ensure;
use pretty_assertions::assert_eq;
use std::process::Command;

#[test]
fn allocator_policy_is_configured_before_cli_threads_start() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let preload = temporary.path().join("allocator-probe.so");
    let source = codex_utils_cargo_bin::find_resource!("tests/fixtures/memory_allocator_probe.c")?;
    let compiled = Command::new("cc")
        .args(["-shared", "-fPIC", "-Wall", "-Wextra", "-Werror"])
        .arg(source)
        .arg("-o")
        .arg(&preload)
        .arg("-ldl")
        .output()?;
    ensure!(compiled.status.success(), "{compiled:?}");
    let result = temporary.path().join("result");
    let output = Command::new(codex_utils_cargo_bin::cargo_bin("codex")?)
        .arg("--version")
        .env("CODEX_HOME", temporary.path())
        .env("LD_PRELOAD", &preload)
        .env("CODEX_TEST_ALLOCATOR_ORDER_RESULT", &result)
        .output()?;
    ensure!(output.status.success(), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(result)?,
        "threads=1 threshold=131072\n"
    );
    Ok(())
}

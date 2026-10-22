//! Verify real Codex OSC titles survive a private tmux server's title forwarding.

use super::focus_palette::PtyCodex;
use super::focus_palette::write_test_config;
use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use std::time::Instant;

struct PrivateTmux(String);

impl PrivateTmux {
    fn command(&self) -> Command {
        let mut command = Command::new("tmux");
        command.args(["-L", &self.0]);
        command
    }
}

impl Drop for PrivateTmux {
    fn drop(&mut self) {
        let _ = self.command().arg("kill-server").output();
    }
}

#[test]
fn tmux_forwards_outdated_and_restored_codex_titles_without_restarting_codex() -> Result<()> {
    let root = tempfile::tempdir_in("/tmp")?;
    let home = tempfile::tempdir_in("/tmp")?;
    let binary = codex_utils_cargo_bin::cargo_bin("codex")?;
    let link = root.path().join("launcher");
    symlink(&binary, &link)?;
    write_test_config(home.path(), root.path())?;
    let config = home.path().join("config.toml");
    let contents = fs::read_to_string(&config)?;
    fs::write(
        config,
        format!("{contents}\n[tui]\nterminal_title = [\"app-name\", \"status\"]\n"),
    )?;
    let server = PrivateTmux(format!(
        "codex-title-{}",
        root.path().file_name().unwrap().to_string_lossy()
    ));
    let launcher = format!("CODEX_LAUNCHER_PATH={}", link.display());
    let mut terminal = PtyCodex::start_binary(
        Path::new("tmux"),
        root.path(),
        home,
        &[
            "-L",
            &server.0,
            "-f",
            "/dev/null",
            "new-session",
            "-s",
            "title",
            "env",
            &launcher,
            binary.to_str().context("native binary path")?,
            "--no-alt-screen",
        ],
        /*editor*/ None,
    )?;
    terminal.wait_for_startup()?;
    ensure!(
        server
            .command()
            .args(["set-option", "-g", "set-titles", "on"])
            .status()?
            .success()
    );
    ensure!(
        server
            .command()
            .args(["set-option", "-g", "set-titles-string", "#T"])
            .status()?
            .success()
    );
    let pane = server
        .command()
        .args(["display-message", "-p", "#{pane_pid}"])
        .output()?
        .stdout;
    let mut titles = Vec::new();
    // /bin/sh is a different native ELF build, never executed as the candidate.
    for (target, expected) in [
        (Path::new("/bin/sh"), "c̶o̶d̶e̶x̶ - Ready"),
        (binary.as_path(), "codex - Ready"),
    ] {
        fs::remove_file(&link)?;
        symlink(target, &link)?;
        let deadline = Instant::now() + Duration::from_secs(/*secs*/ 8);
        loop {
            terminal.read_output(Duration::from_millis(/*millis*/ 50))?;
            if terminal.last_window_title() == Some(expected) {
                break;
            }
            ensure!(
                Instant::now() < deadline,
                "tmux did not forward {expected:?}; last title: {:?}",
                terminal.last_window_title()
            );
        }
        titles.push(terminal.last_window_title().unwrap().to_string());
        let same_pane = server
            .command()
            .args(["display-message", "-p", "#{pane_pid}"])
            .output()?
            .stdout;
        pretty_assertions::assert_eq!(same_pane, pane);
    }
    insta::assert_snapshot!(titles.join("\n"), @r"
    c̶o̶d̶e̶x̶ - Ready
    codex - Ready
    ");
    Ok(())
}

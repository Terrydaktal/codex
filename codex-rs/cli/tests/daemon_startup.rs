//! Exercises per-window startup and resume/fork pickers with Cargo's CLI executable.

use anyhow::Context as _;
use anyhow::Result;
use anyhow::ensure;
use std::collections::VecDeque;
use std::fs;
use std::process::Command;
use std::time::Duration;

#[tokio::test]
#[cfg(unix)]
async fn ordinary_launch_uses_a_per_window_backend() -> Result<()> {
    daemon_startup("start").await
}

#[tokio::test]
#[cfg(unix)]
async fn local_launch_preserves_a_running_daemon_with_different_features() -> Result<()> {
    daemon_startup("mismatch-embedded").await
}

#[tokio::test]
#[cfg(unix)]
async fn local_feature_overrides_leave_stopped_daemon_settings_untouched() -> Result<()> {
    daemon_startup("mismatch-disable-persisted").await
}

#[tokio::test]
async fn daemon_exclusion_survives_resume_picker() -> Result<()> {
    daemon_startup("resume").await
}

#[tokio::test]
async fn daemon_exclusion_survives_fork_picker() -> Result<()> {
    daemon_startup("fork").await
}

#[tokio::test]
async fn daemon_auto_start_preserves_bedrock_onboarding() -> Result<()> {
    daemon_startup("bedrock").await
}

#[tokio::test]
#[cfg(windows)]
async fn restrictive_launcher_uses_the_per_window_default() -> Result<()> {
    const CHILD: &str = "CODEX_TEST_RESTRICTIVE_DAEMON_START";
    if std::env::var_os(CHILD).is_some() {
        // The PTY creates its own breakaway-permitted job. Add Cargo's
        // restriction *inside* the PTY so it is the CLI's innermost job.
        let job = codex_utils_pty::JobObject::create_without_breakaway()?;
        let codex = codex_utils_cargo_bin::cargo_bin("codex")?;
        let mut command = tokio::process::Command::new(codex);
        command
            .arg("--no-alt-screen")
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit());
        let status = job.spawn_contained(&mut command)?.wait().await?;
        ensure!(status.success(), "CLI exited: {status}");
        return Ok(());
    }
    daemon_startup("restrictive-job").await
}

#[tokio::test]
#[cfg(unix)]
async fn bedrock_onboarding_leaves_a_running_daemon_untouched() -> Result<()> {
    daemon_startup("bedrock-running").await
}

async fn daemon_startup(command: &str) -> Result<()> {
    let codex = codex_utils_cargo_bin::cargo_bin("codex")?.canonicalize()?;
    let workspace = tempfile::tempdir()?;
    let workspace_path = workspace.path().canonicalize()?;
    #[cfg(unix)]
    let home = tempfile::Builder::new().tempdir_in("/tmp")?;
    #[cfg(not(unix))]
    let home = tempfile::tempdir()?;
    // Keep cold Rosetta translation outside the timed startup assertions.
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    anyhow::ensure!(
        Command::new(&codex)
            .env("CODEX_HOME", home.path())
            .arg("--version")
            .output()?
            .status
            .success(),
        "failed to prepare CLI test executable"
    );
    fs::write(
        home.path().join("config.toml"),
        format!(
            "model = \"gpt-5.6-terra\"\n\
         features.bedrock_setup_wizard = true\ncli_auth_credentials_store = \"file\"\n\
         suppress_unstable_features_warning = true\nanalytics.enabled = false\n\
         windows.sandbox = \"unelevated\"\n\
         tui.disable_paste_burst = true\n\
         notice.model_migrations.\"gpt-5.6-terra\" = \"gpt-6-sol\"\n\
         [projects.{}]\ntrust_level = \"trusted\"\n",
            serde_json::to_string(&workspace_path)?,
        ),
    )?;
    let mismatch = command.starts_with("mismatch-");
    let persisted = command == "mismatch-disable-persisted";
    let disabling = matches!(command, "mismatch-disable" | "mismatch-disable-persisted");
    let bedrock_onboarding = matches!(command, "bedrock" | "bedrock-running");
    if !bedrock_onboarding {
        fs::write(
            home.path().join("auth.json"),
            r#"{"OPENAI_API_KEY":"daemon-startup-test","tokens":null,"last_refresh":null}"#,
        )?;
    }
    let mut env = std::env::vars().collect::<std::collections::HashMap<_, _>>();
    for key in [
        "CODEX_EXEC_SERVER_URL",
        "CODEX_ACCESS_TOKEN",
        "CODEX_API_KEY",
        "CODEX_CLOUD_TASKS_MODE",
        "OPENAI_API_KEY",
        "OPENAI_FEDERATION_RULE_ID",
        "OPENAI_IDENTITY_TOKEN_FILE",
        "OPENAI_WORKLOAD_IDENTITY_CONTEXT",
    ] {
        env.remove(key);
    }
    env.insert("CODEX_HOME".into(), home.path().display().to_string());
    env.insert(
        "CODEX_SQLITE_HOME".into(),
        home.path().display().to_string(),
    );
    env.insert("TERM".into(), "xterm-256color".into());
    let mut args = vec!["--no-alt-screen".to_string()];
    let mut steps: VecDeque<(&str, &[u8])> = VecDeque::new();
    if matches!(command, "start" | "bedrock-running" | "restrictive-job") || mismatch {
        // A selected package with a stopped daemon avoids installing a release.
        let managed = home
            .path()
            .join("packages/app-server-daemon/current/bin")
            .join(if cfg!(windows) { "codex.exe" } else { "codex" });
        fs::create_dir_all(home.path().join("packages/app-server-daemon/current/bin"))?;
        // Hard links change the executable's ctime and invalidate Rosetta's translation cache.
        #[cfg(unix)]
        std::os::unix::fs::symlink(&codex, &managed)?;
        #[cfg(not(unix))]
        fs::hard_link(&codex, &managed).or_else(|_| fs::copy(&codex, &managed).map(|_| ()))?;
        if command != "restrictive-job" {
            fs::create_dir(home.path().join("app-server-daemon"))?;
            fs::write(
                home.path().join("app-server-daemon/settings.json"),
                if persisted {
                    r#"{"shutdownGraceSeconds":0,"updater":{"autoUpdateEnabled":false},"featureOverrides":{"api_key_model_discovery":true}}"#
                } else {
                    r#"{"shutdownGraceSeconds":0,"updater":{"autoUpdateEnabled":false}}"#
                },
            )?;
        }
    }
    let pid_file = home.path().join("app-server-daemon/daemon.pid");
    let result = async {
        let existing_daemon = if command == "bedrock-running" || (mismatch && !disabling) {
            let started = Command::new(&codex)
                .env_clear()
                .envs(&env)
                .current_dir(&workspace_path)
                .args(["app-server", "daemon", "start"])
                .output()?;
            ensure!(
                started.status.success(),
                "{}",
                String::from_utf8_lossy(&started.stderr)
            );
            Some(fs::read(&pid_file)?)
        } else {
            None
        };
        let expected = if command == "start" {
            // The draft header is visible before the session's command composer is ready.
            steps.push_back(("GPT-5.6-Terra", b"/status\r"));
            "Model:"
        } else if command == "restrictive-job" {
            steps.push_back(("GPT-5.6-Terra", b"/status\r"));
            "Model:"
        } else if mismatch {
            args.extend(if persisted {
                ["--disable".into(), "api_key_model_discovery".into()]
            } else if disabling {
                ["--disable".into(), "auth_elicitation".into()]
            } else {
                ["--enable".into(), "api_key_model_discovery".into()]
            });
            steps.push_back(("GPT-5.6-Terra", b"/status\r"));
            "Model:"
        } else if bedrock_onboarding {
            "UseAmazonBedrock"
        } else {
            args.extend([command.into(), "--strict-config".into()]);
            steps.push_back(("Nosessionsyet", b"\x1b"));
            steps.push_back(("GPT-5.6-Terra", b"/status\r"));
            "Model:"
        };
        let program = if cfg!(windows) && command == "restrictive-job" {
            env.insert("CODEX_TEST_RESTRICTIVE_DAEMON_START".into(), "1".into());
            args = vec![
                "--exact".into(),
                "restrictive_launcher_uses_the_per_window_default".into(),
                "--nocapture".into(),
            ];
            std::env::current_exe()?
        } else {
            codex.clone()
        };
        let spawned = codex_utils_pty::spawn_pty_process(
            &program.to_string_lossy(),
            &args,
            &workspace_path,
            &env,
            /*arg0*/ &None,
            codex_utils_pty::TerminalSize {
                rows: 40,
                cols: 120,
            },
            codex_utils_pty::ChildFds::Inherited(&[]),
        )
        .await?;
        let session = spawned.session;
        let writer = session.writer_sender();
        let mut stdout = spawned.stdout_rx;
        let mut output = String::new();
        let mut screen = vt100::Parser::new(
            /*rows*/ 40, /*cols*/ 120, /*scrollback_len*/ 0,
        );
        let mut queries: Vec<(&str, &[u8])> = vec![
            ("\x1b[6n", b"\x1b[1;1R"),
            ("\x1b[?u", b"\x1b[?0u\x1b[?1;2c"),
            ("\x1b[c", b"\x1b[?1;2c"),
            ("\x1b]10;?", b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\"),
            ("\x1b]11;?", b"\x1b]11;rgb:0000/0000/0000\x1b\\"),
        ];
        let timeout = Duration::from_secs(45);
        let result = tokio::time::timeout(timeout, async {
            while let Some(bytes) = stdout.recv().await {
                output.push_str(&String::from_utf8_lossy(&bytes));
                screen.process(&bytes);
                for (query, reply) in &mut queries {
                    if !query.is_empty() && output.contains(*query) {
                        writer.send(reply.to_vec()).await?;
                        *query = "";
                    }
                }
                let text: String = screen
                    .screen()
                    .contents()
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect();
                if let Some((ready, input)) = steps.front()
                    && text.contains(ready)
                {
                    writer.send(input.to_vec()).await?;
                    steps.pop_front();
                    output.clear();
                } else if steps.is_empty() && text.contains(expected) {
                    ensure!(!text.contains("Server:Localbackgroundserver"));
                    if let Some(existing_daemon) = &existing_daemon {
                        ensure!(fs::read(&pid_file)? == *existing_daemon);
                    } else {
                        ensure!(!pid_file.exists());
                    }
                    if persisted {
                        let settings: serde_json::Value = serde_json::from_slice(&fs::read(
                            home.path().join("app-server-daemon/settings.json"),
                        )?)?;
                        ensure!(settings["featureOverrides"]["api_key_model_discovery"] == true);
                    }
                    return Ok::<_, anyhow::Error>(());
                }
            }
            anyhow::bail!(
                "TUI exited before {expected}: {}",
                screen.screen().contents()
            )
        })
        .await;
        Ok::<_, anyhow::Error>((
            session,
            result.with_context(|| {
                format!(
                    "{command} timed out waiting for {}: {}",
                    steps.front().map_or(expected, |(ready, _)| *ready),
                    screen.screen().contents()
                )
            }),
        ))
    }
    .await;
    // Always stop the detached daemon, including on timeout or failed assertions.
    let stopped = Command::new(&codex)
        .env_clear()
        .envs(&env)
        .args(["app-server", "daemon", "stop"])
        .output()?;
    let (session, result) = result?;
    session.terminate();
    result??;
    ensure!(
        stopped.status.success(),
        "{}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    Ok(())
}

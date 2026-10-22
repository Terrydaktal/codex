//! Exercises the per-window default through real TUI startup with a listening daemon socket.

use super::focus_palette::PtyCodex;
use super::focus_palette::write_test_config;
use anyhow::Result;
use anyhow::ensure;
use pretty_assertions::assert_eq;
use std::os::unix::net::UnixListener;

#[test]
fn normal_launches_ignore_existing_daemon_and_its_package() -> Result<()> {
    for args in [
        &[][..],
        &["-c", "features.daemon_auto_start=true"][..],
        &["-c", "features.daemon_auto_start=false"][..],
        &["-c", "features.api_key_model_discovery=true"][..],
    ] {
        let cwd = codex_utils_cargo_bin::repo_root()?;
        let home = tempfile::tempdir_in("/tmp")?;
        write_test_config(home.path(), &cwd)?;
        let config = home.path().join("config.toml");
        let contents = std::fs::read_to_string(&config)?;
        std::fs::write(
            config,
            contents.replace("features.daemon_auto_start = false\n", ""),
        )?;
        // Even an incomplete old daemon package must not affect a local launch.
        std::fs::create_dir_all(home.path().join("packages/app-server-daemon/current"))?;
        let daemon_state = home.path().join("app-server-daemon");
        let socket = codex_app_server_client::app_server_control_socket_path(home.path())?;
        std::fs::create_dir_all(socket.parent().unwrap())?;
        let listener = UnixListener::bind(socket.as_path())?;
        listener.set_nonblocking(true)?;
        let mut terminal = PtyCodex::start(&cwd, home, args)?;
        terminal.wait_for_startup()?;
        terminal.write_input(b"/status")?;
        terminal.wait_for_screen("show current session configuration")?;
        terminal.read_output(std::time::Duration::from_millis(/*millis*/ 200))?;
        terminal.write_input(b"\r")?;
        terminal.wait_for_screen("Model:")?;
        ensure!(!terminal.screen_contains("unix://"));
        ensure!(!terminal.screen_contains("for agents"));
        ensure!(!daemon_state.exists(), "local launch created daemon state");
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        ensure!(!terminal.screen_contains("Running without the shared background server"));
    }
    Ok(())
}

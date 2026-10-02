//! Cover shared probing, session targeting, and conservative behavior for unknown mouse settings.

use super::*;
use pretty_assertions::assert_eq;

#[cfg(unix)]
fn write_tmux_probe(executable: &Path) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::write(
        executable,
        "#!/bin/sh\n[ \"$1\" = display-message ] && [ \"$3\" = -t ] && [ \"$4\" = %42 ] || exit 1\nprintf 'csi-u\\toff\\n'\n",
    )
    .expect("write installed tmux probe");
    std::fs::set_permissions(executable, std::fs::Permissions::from_mode(/*mode*/ 0o755))
        .expect("make tmux probe executable");
}

#[cfg(unix)]
#[test]
fn user_installed_tmux_symlink_probes_the_containing_pane_before_system_fallback() {
    let home = tempfile::tempdir().expect("user home");
    let build = tempfile::tempdir().expect("build outside user installation directory");
    let executable = build.path().join("tmux");
    write_tmux_probe(&executable);
    let local_bin = home.path().join(".local/bin");
    std::fs::create_dir_all(&local_bin).expect("user installation directory");
    std::os::unix::fs::symlink(&executable, local_bin.join("tmux")).expect("install tmux symlink");

    assert_eq!(
        options_for_installation(Some("%42"), Some(home.path()), || {
            panic!("a working user installation must take precedence over system tmux")
        }),
        Options {
            extended_keys_format: Some("csi-u".to_owned()),
            mouse_capture: MouseCapture::DisabledByTmux,
        },
    );
}

#[cfg(unix)]
#[test]
fn unavailable_user_installations_preserve_the_system_probe() {
    use std::os::unix::fs::PermissionsExt;

    #[derive(Debug)]
    enum Installation {
        Missing,
        Directory,
        NonExecutable,
        BrokenLink,
    }

    let installations = tempfile::tempdir().expect("installations");
    let system_tmux = installations.path().join("system-tmux");
    write_tmux_probe(&system_tmux);
    for installation in [
        Installation::Missing,
        Installation::Directory,
        Installation::NonExecutable,
        Installation::BrokenLink,
    ] {
        let home = installations.path().join(format!("{installation:?}"));
        let local_bin = home.join(".local/bin");
        std::fs::create_dir_all(&local_bin).expect("user installation directory");
        let executable = local_bin.join("tmux");
        match installation {
            Installation::Directory => {
                std::fs::create_dir(&executable).expect("not an executable");
            }
            Installation::NonExecutable => {
                write_tmux_probe(&executable);
                std::fs::set_permissions(
                    &executable,
                    std::fs::Permissions::from_mode(/*mode*/ 0o644),
                )
                .expect("remove execute permission");
            }
            Installation::BrokenLink => {
                std::os::unix::fs::symlink(local_bin.join("absent"), &executable)
                    .expect("broken tmux symlink");
            }
            Installation::Missing => {}
        }
        let mut fallback_called = false;
        let options = options_for_installation(Some("%42"), Some(&home), || {
            fallback_called = true;
            Some(system_tmux.clone())
        });
        assert_eq!(
            (fallback_called, options),
            (
                true,
                Options {
                    extended_keys_format: Some("csi-u".to_owned()),
                    mouse_capture: MouseCapture::DisabledByTmux,
                },
            ),
            "invalid user installation: {installation:?}",
        );
    }
}

#[test]
fn absent_or_relative_home_does_not_discover_a_workspace_helper() {
    for home in [None, Some(Path::new("relative-home"))] {
        let mut fallback_called = false;
        let options = options_for_installation(Some("%42"), home, || {
            fallback_called = true;
            None
        });
        assert_eq!((fallback_called, options), (true, Options::default()));
    }
}

#[test]
fn input_probe_targets_the_containing_pane_and_only_disables_confirmed_mouse_off() {
    for (mouse, mouse_capture) in [
        ("0", MouseCapture::DisabledByTmux),
        ("off", MouseCapture::DisabledByTmux),
        ("1", MouseCapture::Enabled),
        ("on", MouseCapture::Enabled),
        ("", MouseCapture::Enabled),
        ("unknown", MouseCapture::Enabled),
    ] {
        let mut calls = Vec::new();
        let options = read_options(Some("%42"), |args| {
            calls.push(args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>());
            Some(format!("csi-u\t{mouse}\n").into_bytes())
        });
        assert_eq!(
            options,
            Options {
                extended_keys_format: Some("csi-u".to_owned()),
                mouse_capture,
            }
        );
        assert_eq!(
            calls,
            vec![vec![
                "display-message",
                "-p",
                "-t",
                "%42",
                "#{extended-keys-format}\t#{mouse}",
            ]]
        );
    }
}

#[test]
fn keyboard_fallback_preserves_confirmed_mouse_policy_without_guessing_on_failure() {
    for (output, mouse_capture) in [
        (Some(b"\t0\n".to_vec()), MouseCapture::DisabledByTmux),
        (Some(b"\t\n".to_vec()), MouseCapture::Enabled),
        (Some(vec![0xff]), MouseCapture::Enabled),
        (None, MouseCapture::Enabled),
    ] {
        let mut calls = Vec::new();
        let options = read_options(/*pane*/ None, |args| {
            calls.push(args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>());
            if args[0] == "display-message" {
                output.clone()
            } else {
                Some(b"xterm\n".to_vec())
            }
        });
        assert_eq!(
            options,
            Options {
                extended_keys_format: Some("xterm".to_owned()),
                mouse_capture,
            }
        );
        assert_eq!(
            calls,
            vec![
                vec!["display-message", "-p", "#{extended-keys-format}\t#{mouse}"],
                vec!["show-options", "-gqv", "extended-keys-format"],
            ]
        );
    }
    assert_eq!(read_options(Some("%42"), |_| None), Options::default());
}

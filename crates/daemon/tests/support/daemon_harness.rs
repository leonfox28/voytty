//! Multi-process daemon test-harness path encoding and child entry.

use std::ffi::OsString;
use std::path::PathBuf;

use zterm_platform::user_state::UserPaths;

const CHILD_PREFIX: &str = "--zterm-test-daemon=";
const FOREGROUND_CHILD_PREFIX: &str = "--zterm-test-foreground-daemon=";
const TERMINAL_CHILD_PREFIX: &str = "--zterm-test-terminal-daemon=";
const SLOW_SOCKET_CHILD_PREFIX: &str = "--zterm-test-slow-socket-daemon=";
const HANDOFF_OWNER_PREFIX: &str = "--zterm-test-handoff-owner=";
/// Longer than one readiness probe, well inside the launch budget.
const SLOW_SOCKET_DELAY: std::time::Duration = std::time::Duration::from_millis(750);

/// Encodes task-private paths into the one detached child harness argument.
#[allow(dead_code)]
pub fn child_argument(paths: &UserPaths) -> String {
    format!("{CHILD_PREFIX}{}", encode_paths(paths))
}

/// Explicit service-manager-style child retaining its process/session identity.
#[allow(dead_code)]
pub fn foreground_argument(paths: &UserPaths) -> String {
    format!("{FOREGROUND_CHILD_PREFIX}{}", encode_paths(paths))
}

/// Hidden detached-child argument for a daemon with a deterministic PTY fixture.
#[allow(dead_code)]
pub fn terminal_child_argument(paths: &UserPaths) -> String {
    format!("{TERMINAL_CHILD_PREFIX}{}", encode_paths(paths))
}

/// Detached child whose socket accepts connections but answers nothing for
/// [`SLOW_SOCKET_DELAY`], as a loaded host can between bind and serve.
#[allow(dead_code)]
pub fn slow_socket_child_argument(paths: &UserPaths) -> String {
    format!("{SLOW_SOCKET_CHILD_PREFIX}{}", encode_paths(paths))
}

/// Launcher for `daemon_argument` whose executable also serves the hidden
/// lifecycle handoff entry. That entry carries no paths, so the script binds it
/// to these task-private paths; every other argument reaches this harness.
#[allow(dead_code)]
pub fn launcher(
    paths: &UserPaths,
    daemon_argument: &str,
) -> zterm_daemon::lifecycle::DaemonLauncher {
    use std::os::unix::fs::PermissionsExt;

    let harness = std::env::current_exe().expect("harness executable");
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\"'\"'"));
    let harness = quote(harness.to_str().expect("UTF-8 harness path"));
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = {} ]; then exec {harness} {}; fi\nexec {harness} \"$@\"\n",
        zterm_daemon::handoff::INTERNAL_HANDOFF_ARGUMENT,
        quote(&format!("{HANDOFF_OWNER_PREFIX}{daemon_argument}")),
    );
    let executable = paths.home().with_file_name("zterm-launcher");
    // Rename keeps a launcher another process is executing whole.
    let staged = executable.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&staged, script).expect("launcher script");
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o700))
        .expect("launcher mode");
    std::fs::rename(&staged, &executable).expect("launcher publication");
    zterm_daemon::lifecycle::DaemonLauncher::for_test(executable, daemon_argument.to_owned())
}

/// Encodes task-private paths for another test-only child role.
pub fn encode_paths(paths: &UserPaths) -> String {
    format!(
        "{}:{}:{}:{}",
        paths.uid(),
        encode_path(paths.home()),
        encode_path(paths.state_root()),
        encode_path(paths.runtime_dir())
    )
}

/// Runs the daemon or lifecycle owner child when the custom harness
/// executable received its child argument.
pub fn run_child_if_requested() -> bool {
    let Some(argument) = std::env::args().nth(1) else {
        return false;
    };
    if let Some(daemon_argument) = argument.strip_prefix(HANDOFF_OWNER_PREFIX) {
        run_handoff_owner(daemon_argument);
        return true;
    }
    let (encoded, deterministic_terminal, foreground, slow_socket) =
        if let Some(encoded) = argument.strip_prefix(CHILD_PREFIX) {
            (encoded, false, false, false)
        } else if let Some(encoded) = argument.strip_prefix(FOREGROUND_CHILD_PREFIX) {
            (encoded, false, true, false)
        } else if let Some(encoded) = argument.strip_prefix(TERMINAL_CHILD_PREFIX) {
            (encoded, true, false, false)
        } else if let Some(encoded) = argument.strip_prefix(SLOW_SOCKET_CHILD_PREFIX) {
            (encoded, false, false, true)
        } else {
            return false;
        };
    let paths = match decode_paths(encoded) {
        Ok(paths) => paths,
        Err(detail) => {
            eprintln!("invalid daemon harness paths: {detail}");
            std::process::exit(2);
        }
    };
    let _diagnostics = zterm_daemon::diagnostics::install(&paths);
    let result = (if foreground {
        Ok(())
    } else {
        zterm_platform::local_unix::detach_current_process()
    })
    .map_err(|error| error.to_string())
    .and_then(|()| {
        // Never accepted: readiness connects into the backlog and waits.
        // Unlinking, not closing, keeps those connections pending as a slow
        // daemon's would, while the daemon below binds the path afresh.
        let _unanswered = if slow_socket {
            paths
                .prepare_runtime_directory()
                .map_err(|error| error.to_string())?;
            let listener = std::os::unix::net::UnixListener::bind(paths.socket())
                .map_err(|error| error.to_string())?;
            std::thread::sleep(SLOW_SOCKET_DELAY);
            std::fs::remove_file(paths.socket()).map_err(|error| error.to_string())?;
            Some(listener)
        } else {
            None
        };
        if deterministic_terminal {
            let sessions = terminal_fixture_sessions(&paths)?;
            zterm_daemon::lifecycle::run_local_only_daemon_with_sessions_for_test(&paths, sessions)
                .map_err(|error| error.to_string())
        } else {
            zterm_daemon::lifecycle::run_local_only_daemon_for_test(&paths)
                .map_err(|error| error.to_string())
        }
    });
    if let Err(error) = result {
        eprintln!("daemon harness failed: {error}");
        std::process::exit(1);
    }
    true
}

fn run_handoff_owner(daemon_argument: &str) {
    let paths = [
        CHILD_PREFIX,
        FOREGROUND_CHILD_PREFIX,
        TERMINAL_CHILD_PREFIX,
        SLOW_SOCKET_CHILD_PREFIX,
    ]
    .into_iter()
    .find_map(|prefix| daemon_argument.strip_prefix(prefix))
    .ok_or_else(|| "unknown daemon argument".to_owned())
    .and_then(decode_paths)
    .unwrap_or_else(|detail| {
        eprintln!("invalid handoff owner paths: {detail}");
        std::process::exit(2);
    });
    let runtime = zterm_daemon::operations::LocalRuntime::for_test(
        paths,
        zterm_daemon::lifecycle::DaemonLauncher::for_test(
            std::env::current_exe().expect("harness executable"),
            daemon_argument.to_owned(),
        ),
    );
    if zterm_daemon::handoff::run_internal_handoff_for_test(&runtime).is_err() {
        std::process::exit(1);
    }
}

fn terminal_fixture_sessions(
    paths: &UserPaths,
) -> Result<zterm_daemon::session::SessionService, String> {
    let own_device_id = zterm_daemon::identity::DeviceIdentity::load(paths)
        .map_err(|error| error.to_string())?
        .device_id();
    let shell = [PathBuf::from("/bin/sh"), PathBuf::from("/usr/bin/sh")]
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| "deterministic terminal fixture requires a POSIX shell".to_owned())?;
    let default_working_directory = paths.home().to_path_buf();
    Ok(zterm_daemon::session::SessionService::with_spawner(
        own_device_id,
        zterm_core::ResourceLimits::default(),
        move |size, requested_working_directory| {
            let working_directory = requested_working_directory
                .map(PathBuf::from)
                .unwrap_or_else(|| default_working_directory.clone());
            let session = zterm_platform::pty::PtyHost::new()
                .spawn(
                    zterm_platform::pty::ExplicitPtyCommand::new(&shell, &working_directory)
                        .arg("-c")
                        .arg(
                            "printf 'ZTERM_LOCAL_UI_SHELL_READY\\r\\n'; while IFS= read -r line; do case \"$line\" in ZTERM_TEST_KEYBOARD_15) printf '\\033[=15uZTERM_KEYS_READY\\r\\n' ;; ZTERM_TEST_NOTIFICATIONS) printf '\\033]9;结果: done\\a\\033]777;notify;标题;body;结果\\033\\\\' ;; ZTERM_TEST_DECSET_1049) printf '\\033[?1049hZTERM_ALT_READY\\r\\n' ;; ZTERM_TEST_DECRST_1049) printf '\\033[?1049lZTERM_MAIN_READY\\r\\n' ;; *) printf '%s\\r\\n%s\\r\\n' \"$line\" \"$line\" ;; esac; done",
                        ),
                    zterm_platform::pty::PtySize::new(size.rows, size.columns),
                )
                .map_err(|_| {
                    zterm_daemon::error::DaemonError::new(
                        zterm_core::DomainErrorKind::StoreUnavailable,
                        "deterministic terminal fixture could not start",
                    )
                })?;
            Ok((session, working_directory))
        },
    ))
}

/// Decodes task-private paths carried by a test-only child role.
pub fn decode_paths(encoded: &str) -> Result<UserPaths, String> {
    let fields = encoded.split(':').collect::<Vec<_>>();
    if fields.len() != 4 {
        return Err("expected uid and three hex paths".to_owned());
    }
    let uid = fields[0]
        .parse::<u32>()
        .map_err(|error| format!("invalid UID: {error}"))?;
    let home = decode_path(fields[1])?;
    let state = decode_path(fields[2])?;
    let runtime = decode_path(fields[3])?;
    Ok(UserPaths::for_test(uid, home, state, runtime))
}

#[cfg(unix)]
fn encode_path(path: &std::path::Path) -> String {
    use std::os::unix::ffi::OsStrExt;

    let mut encoded = String::with_capacity(path.as_os_str().as_bytes().len() * 2);
    for byte in path.as_os_str().as_bytes() {
        use std::fmt::Write;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

#[cfg(not(unix))]
fn encode_path(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(unix)]
fn decode_path(encoded: &str) -> Result<PathBuf, String> {
    use std::os::unix::ffi::OsStringExt;

    if !encoded.len().is_multiple_of(2) {
        return Err("hex path has an odd length".to_owned());
    }
    let mut bytes = Vec::with_capacity(encoded.len() / 2);
    let (pairs, remainder) = encoded.as_bytes().as_chunks::<2>();
    if !remainder.is_empty() {
        return Err("hex path has an odd length".to_owned());
    }
    for pair in pairs {
        let pair = std::str::from_utf8(pair).map_err(|error| error.to_string())?;
        bytes.push(u8::from_str_radix(pair, 16).map_err(|error| error.to_string())?);
    }
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

#[cfg(not(unix))]
fn decode_path(encoded: &str) -> Result<PathBuf, String> {
    Ok(PathBuf::from(OsString::from(encoded)))
}

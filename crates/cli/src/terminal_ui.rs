//! Desktop raw-terminal ownership and semantic terminal presentation.

use super::{CliError, TerminalRequest};
use voytty_daemon::operations::LocalRuntime;

/// Runs one deferred terminal request after validating the physical terminal.
pub async fn run_terminal(
    request: TerminalRequest,
    runtime: &LocalRuntime,
) -> Result<(), CliError> {
    #[cfg(any(unix, windows))]
    {
        let context = match &request.kind {
            super::TerminalRequestKind::Attach {
                target,
                selector: Some(selector),
                ..
            } => Some((target.clone(), selector.clone())),
            _ => None,
        };
        desktop::run(request, runtime)
            .await
            .map_err(|error| match context {
                Some((target, selector)) => error.with_session(target, selector),
                None => error,
            })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (request, runtime);
        Err(CliError::Daemon(voytty_daemon::error::DaemonError::new(
            voytty_core::DomainErrorKind::UnsupportedPlatform,
            "interactive terminal attachment is not supported on this platform",
        )))
    }
}

#[cfg(any(unix, windows))]
#[path = "terminal_ui/desktop.rs"]
mod desktop;

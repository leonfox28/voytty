//! Cross-platform operating-system integration for voytty.

pub mod account;
#[cfg(unix)]
pub mod autostart;
#[cfg(any(unix, windows))]
pub mod diagnostics;
#[cfg(unix)]
pub mod local_unix;
pub mod pty;
#[cfg(any(unix, windows))]
pub mod upload;
pub mod user_state;

/// Platform facts safe to expose from the side-effect-free Foundation probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlatformFacts {
    /// Rust's compile-time operating-system identifier.
    pub os: &'static str,
    /// Rust's compile-time CPU architecture identifier.
    pub arch: &'static str,
}

/// Returns compile-time platform facts without probing or mutating the host.
#[must_use]
pub const fn current() -> PlatformFacts {
    PlatformFacts {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
    }
}

/// Returns the shared workspace build identity.
#[must_use]
pub const fn build_identity() -> voytty_core::BuildIdentity {
    voytty_core::BuildIdentity::current()
}

#[cfg(unix)]
pub use local_unix as local;
#[cfg(windows)]
#[path = "local_windows.rs"]
pub mod local;
#[cfg(windows)]
mod windows_security;

#[cfg(windows)]
#[path = "autostart_windows.rs"]
pub mod autostart;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_platform_is_non_empty() {
        let platform = current();
        assert!(!platform.os.is_empty());
        assert!(!platform.arch.is_empty());
    }
}

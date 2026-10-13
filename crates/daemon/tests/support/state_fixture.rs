//! Isolated effective-user state fixture shared by daemon integration tests.

use std::fs;

use zterm_platform::user_state::UserPaths;

/// Temporary managed user-state paths.
pub struct TestState {
    _temporary: tempfile::TempDir,
    /// Product paths rooted inside the fixture.
    pub paths: UserPaths,
}

impl TestState {
    /// Creates an empty state fixture owned by the effective UID.
    pub fn new() -> Self {
        #[cfg(unix)]
        let temporary = {
            use std::os::unix::fs::PermissionsExt;
            tempfile::Builder::new()
                .permissions(fs::Permissions::from_mode(0o700))
                .tempdir()
                .expect("private temporary state root")
        };
        #[cfg(not(unix))]
        let temporary = tempfile::tempdir().expect("temporary state root");
        let home = temporary.path().join("home");
        // The fixture models a private account home even when the caller uses
        // a group-writable umask. Keep the production path checks unchanged.
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&home)
                .expect("test home");
        }
        #[cfg(not(unix))]
        fs::create_dir(&home).expect("test home");
        #[cfg(unix)]
        let uid = nix::unistd::Uid::effective().as_raw();
        #[cfg(not(unix))]
        let uid = 0;
        let paths = UserPaths::for_test(
            uid,
            home.clone(),
            home.join(".zterm"),
            temporary.path().join("run"),
        );
        Self {
            _temporary: temporary,
            paths,
        }
    }
}

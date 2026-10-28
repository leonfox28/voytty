# Local portable-pty 0.9.0 ConPTY initialization fix

This is the crates.io `portable-pty` 0.9.0 source with its MIT license.
Upstream: <https://github.com/wezterm/wezterm>, revision
`f8921727a11b9f8b073e8c24821d72fd41283500` (`pty/`). Published crate SHA-256:
`b4a596a2b3d2752d94f51fac2d4a96737b8705dddd311a32b9af47211f08671e`.
The registry installation marker is omitted.

The only source change removes `PSEUDOCONSOLE_INHERIT_CURSOR` from Windows
pseudoconsole creation. Voytty owns a fresh semantic terminal whose initial cursor
is independent of any frontend. Inheriting the creator's cursor instead emits a
DSR query and can deadlock early session teardown when that query has not been
answered, including while a large input write is blocked. This is documented in
Microsoft's [pseudoconsole teardown requirements](https://learn.microsoft.com/en-us/windows/console/creating-a-pseudoconsole-session#ending-the-pseudoconsole-session).
The existing resize/input flags and Unix implementation are unchanged.

`crates/platform/tests/windows_runtime.rs` requires startup without an inherited
cursor query, and independently requires blocked 4 MiB input to complete and
output to reach EOF within five seconds of explicit close. Both native Windows
architectures run it through `just ci-windows`. The detached daemon fixture also
creates, attaches, detaches, reattaches and closes a real PowerShell session.

Remove this patch when upstream exposes cursor inheritance as an option that
Voytty can disable, preserving those native regressions.

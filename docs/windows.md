# Windows support

voytty targets native `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc`.
32-bit x86 and ARM are outside this implementation. Windows uses the same CLI,
semantic terminal model, Iroh pairing, authorization, remote sessions and upload
protocol as Unix. The executable statically links the C runtime.
Both Windows executable targets reserve 8 MiB of stack for CLI/daemon entry
points, including the larger unoptimized async lifecycle futures. This is virtual
address reservation; stack pages are committed on demand. See Microsoft's
[/STACK option](https://learn.microsoft.com/en-us/cpp/build/reference/stack-stack-allocations).

ConPTY requires Windows 10 version 1809 or newer. The native CI baseline is
Windows Server 2025 x64 and Windows 11 ARM64. Cross-compilation on macOS checks
Rust types and target dependencies; native CI owns runtime evidence. Automated
console encoding tests and ConPTY tests do not replace interactive review in
Windows Terminal, including IME, selection, clipboard and console restoration.
[Microsoft's ConPTY documentation](https://learn.microsoft.com/en-us/windows/console/creating-a-pseudoconsole-session)
explains the platform API and the need to service input/output independently.

## Host and local account boundary

The Windows account is resolved from the process token SID and Windows known
profile folder, independent of HOME/USERPROFILE overrides. A hosted Session starts
system Windows PowerShell (`-NoLogo`, with its ordinary profile) in that profile
folder or an explicit `--cwd`. Dimensions must fit ConPTY's signed 16-bit range.
Closing a Session closes its ConPTY on a separate thread while output continues
to drain; this releases blocked input and attached processes. It also accommodates
[ClosePseudoConsole's blocking behavior on older Windows versions](https://learn.microsoft.com/en-us/windows/console/closepseudoconsole).
The pinned [portable-pty patch](../vendor/portable-pty/VOYTTY.md) disables cursor
inheritance so new hosted terminals can close even before their first output has
been processed; startup never depends on a frontend's cursor report.

State is stored in `<profile>/.voytty`, with the endpoint marker in
`<profile>/.voytty-run`. Managed directories/files use protected user/SYSTEM DACLs.
Other users' grants, reparse points and foreign ownership are rejected. The owner
must be the user or Administrators (which Windows can assign during elevated
creation). The DACL must admit the user and no ordinary foreign account;
Administrators remain part of the OS trust boundary even after returning to an
unelevated process. New objects
are created only beneath parents whose mutation rights are limited to the current
account, Administrators and SYSTEM. Numeric compatibility UIDs are never used for
Windows authorization.

Local requests use a namespaced named pipe derived from the full SID and endpoint
path. The DACL admits the account; both ends authenticate the peer process token
before protocol traffic. A bounded record layer carries directional FIN because
named pipes do not provide Unix socket half-close. Flush/shutdown wait until the
writer has delivered accepted records. Lifecycle handoff uses a separate random,
local-only pipe and verifies the exact spawned child process.

The daemon runs detached with null standard handles. `daemon autostart enable`
opts into the current user's `Run` registry value `VoyttyDaemon`; status/disable
inspect or remove that value. It is not a system service. There is no autostart
registration during installation or setup.

## Frontend and files

The Windows frontend uses console events for keys, mouse and resize, with VT output
around the shared renderer. Input epochs, local command mode and remote recovery
keep their shared ownership rules. Ctrl-C as raw input reaches the hosted shell;
console close/break signals trigger frontend cleanup. Pairing's hidden input mode
restores the original console mode after reading.

Uploaded files live under `<profile>/voytty-upload-<SID>/<session>/<transfer>/`.
Returned Windows paths use forward slashes and single quoting for PowerShell,
including profiles with spaces or Unicode. Paths with control or shell syntax
characters are rejected before paste. No Enter key is sent automatically.

Update stages the replacement next to the installed executable, retains the old
file by rename, and restores it if activation fails. Windows can retain a running
image mapping after rename; committed old executables and uninstalled executables
are deleted by a system PowerShell helper after their processes exit. That helper
waits for at most two minutes and only receives the exact retired file path as
child environment data. A process that retains a mapping longer can leave a retired
file for manual cleanup. Uninstall does not modify unrelated PATH entries.

## Verification

`just ci-windows` runs workspace Clippy, shared tests, Windows platform acceptance,
a detached-daemon/PowerShell Session test, script syntax checks and selected patched
transport tests. The daemon fixture uses only disposable state. The platform tests
cover ACL rejection, locks/atomic writes, pipe FIN/drop delivery, PTY input/resize,
exit status and blocked input cleanup. `just windows-build` adds native PE and
embedded identity checks. Signed installer acceptance runs on both architectures
before publication; no release is published by ordinary CI or this feature task.

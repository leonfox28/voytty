# iOS

The product target is **interface and feature parity with the current Android
app, implemented with SwiftUI/UIKit**. The existing connection/terminal slice is
a foundation, not the final scope. Follow the [Android parity checklist](ios-parity.md)
for the implementation order and acceptance criteria. Keep iOS-native text
selection, handles, Copy menu, keyboard and platform pickers. The shared Rust
client and terminal bridge remain the protocol/state owners on both platforms.

The iOS client supports camera/image/manual ticket pairing, saved hosts, listing and
creating Sessions, confirmed takeover, semantic terminal rendering, scrollback,
system IME input, native text selection/copy, paste, image/file uploads, shortcut keys and detach/reattach. It targets iOS 17+ on
iPhone and iPad. This is a development app; no App Store or TestFlight package
is published by this change.

## Build and run

Use Apple Silicon macOS, full Xcode with an installed iOS simulator runtime,
Python 3.11+, XcodeGen 2.46+ and the repository's Rust 1.98.0 toolchain. The initial
local environment uses Xcode 27.1 and an iOS 27.0 simulator.

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim --toolchain 1.98.0
just ios-project
open apps/ios/Voytty.xcodeproj
just ios-build
just ios-device-build
xcrun simctl list devices available
just ios-check <simulator-uuid>
```

Select the **Voytty** scheme in Xcode. Simulator builds use ad-hoc signing, which
also supplies the application identity required by Keychain. Do not disable
signing for simulator runtime tests. `ios-device-build` cross-builds the device
app without signing; it does not install an app or prove phone behavior. To run
on an iPhone, select your development team under Signing & Capabilities in Xcode
and choose that phone as the destination. Signing credentials stay outside Git.

For interactive review, keep the device window in the foreground. Command-line
builds, XCTest and XCUITest may also run headlessly, including while the Mac is
locked; record runtime checks separately from visual/manual acceptance. Xcode 27 displays
simulators in **Xcode → Open Developer Tool → Device Hub**, rather than the old
Simulator app. Select the booted iPhone and open **voytty Dev**. Tests disable
parallel device cloning so the explicit destination is also the visible device.
**Device → Keyboard → Toggle Software Keyboard** shows the on-screen keyboard;
use its globe button to switch between English and Chinese Pinyin.

Debug uses `io.github.leonfox28.voytty.dev` / **voytty Dev**; release uses
`io.github.leonfox28.voytty` / **voytty**. Their identities and saved hosts are
independent. Product version comes from the root Cargo workspace; build number 1
is only a development placeholder until an iOS distribution workflow is added.

`tools/ios/build.py` generates the Swift bindings, module map, version config and
Xcode project. Xcode's pre-build step compiles the selected Rust target as a
static library and copies only that library into the build products. Generated
bindings, projects and build products are ignored. Do not hand-edit them.
Rust outputs live in `target/ios-native` and Xcode outputs in `target/ios`
(beneath `CARGO_TARGET_DIR` when configured). This isolates stable-name FFI
libraries from workspace/Android builds that enable different Cargo features.

## Use

1. On the host, run `voytty pair create`.
2. Tap **添加主机** and scan the pairing QR, choose an image through the system
   photo picker, or use **手动输入 → 配对并连接**. When an image contains multiple
   valid pairing codes, choose the device by name and ID suffix. Saving a pairing
   opens that host using the same entry policy as a saved host.
3. Choose a saved host or the recent connection card. The app opens its remembered
   Session when still present; otherwise it opens the only free Session, offers
   the Session panel for multiple/occupied Sessions, or opens the default `main`
   after a successful empty list. Tap the bottom Session chip for a native sheet
   with medium and large heights. Drag up to expand, drag down or tap Done to
   close, and pull the list to refresh. The navigation bar's plus button creates
   a Session; the form accepts an optional host directory. Each row offers Rename
   and confirmed Delete. The row menu stays beside its button; Delete opens a centered system
   alert naming the Session, explaining that its processes will end, and offering
   Cancel and destructive Delete. Occupied Sessions and host removal use the same
   centered system-alert style, naming the target and requiring explicit confirmation.
   Connection failure, occupancy, loss of control and Session end use system
   alerts. Retry or confirmed takeover acts on the current connection context;
   dismissing an alert leaves remote tasks alone. Ended Sessions offer the list
   instead of creating a replacement through Retry. Automatic reconnect keeps
   its progress indicator without repeatedly opening alerts.
4. Use the keyboard button to show/hide the system keyboard. Hardware keyboards
   continue working while it is hidden. Esc, Ctrl and arrows
   are available above it. Long-press the Session chip for Paste, Tab and Alt;
   Ctrl and Alt apply to the next input. Paste uses bracketed paste when the
   remote application requests that mode.
   Hardware Ctrl/Option combinations, navigation and F1–F12 use terminal key
   events; Command shortcuts, Control-Space and active IME composition stay with
   UIKit. Session forms and pickers temporarily own keyboard focus. Uncommitted
   IME text is discarded when input pauses or the attachment changes.
5. Swipe vertically to browse retained output. The circular down-arrow button
   at the terminal's bottom center (**回到最新输出**) returns to live output and
   clears the selection. It uses the system Liquid Glass button style on iOS 26
   and later, with a filled circular button on earlier versions.
   The system navigation bar shows the connected host above its connection
   status, Direct/Relay path and latency; these details are no longer repeated
   in the Session list. Back is always available at the top
   left. Its reserved layout space keeps terminal rows below the bar. Tap Back
   or swipe right from the left edge to return to the host list without ending
   the remote shell; the recent card reconnects to it. Cancelling a partial edge
   swipe leaves the connection in place. Settings and pairing use the same
   native navigation stack.
6. Long-press output, drag the system selection handles, then choose the system
   **Copy** action. A selection wholly inside one terminal HTTP(S) hyperlink also
   offers **Open Link** in that same native menu; it revalidates the captured
   selection before handing the URL to iOS. Hold a handle near the terminal's upper or lower edge to
   scroll and extend the selection across screens. Lifting the finger stops
   scrolling. Use the keyboard button to resume typing.

7. Tap the photo or paperclip button to choose one image or file using the native
   iOS picker. The limit is 50 MB (50,000,000 bytes), including empty files. The
   system sheet shows the host-accepted bytes and transfer speed, with Cancel
   in its navigation bar. Failures use an iOS alert with Retry and Close. The
   picker and progress sheet finish dismissing before another presentation or
   terminal keyboard focus is admitted. A successful upload inserts the remote
   path once into the original Session and never sends Enter.

On the normal screen, touch dragging browses history and a quick flick coasts
until it slows down or reaches the history boundary. A new touch, selection,
keyboard input, geometry/authority change or backgrounding stops the coast.
Trackpad and mouse-wheel scrolling are supported; the app does not add touch
inertia to these already-delivered scroll events.

On a live grid with mouse reporting enabled, taps send a primary-button
press/release and vertical drags send bounded wheel reports at the touched cell.
When the child enables alternate-screen scrolling, a vertical drag becomes cursor
keys through the shared Rust encoder; taps in that mode send nothing. Local
history and long-press selection retain their own gestures. Each remote gesture
captures the displayed source, cell, mode, input epoch and geometry, so a resize,
reconnect, mode change or selection cannot redirect an old coordinate to a new
grid. Pointer events share the ordered input writer with keystrokes.
The remote single tap takes precedence over UIKit's single-tap focus action;
UIKit's long press, multi-tap selection and handles retain priority. These
relationships use the public
[text-interaction gesture list](https://developer.apple.com/documentation/uikit/uitextinteraction/gesturesforfailurerequirements),
without replacing UIKit recognizer delegates or selection controls.

The terminal uses the same four LXGW Bright Code NF GB font files as Android,
preserves the host's cell widths
and colors, and fits wide glyphs into their declared cells. A frame repaints only
the rows and cursor cells that changed, rounded out to whole device pixels;
repeated frames, unrelated SwiftUI updates and cursor blinking do not redraw the
grid. Glyphs are shaped once per text and face. UIKit owns Chinese
and other marked-text composition; only committed text is sent. Input captures
its synchronization epoch and is discarded when that epoch is no longer valid.
The shared Rust driver owns automatic reconnect while the attachment is alive.

Keyboard frame notifications and the window's rotation transition defer remote
viewport changes until layout settles. The grid remains anchored at the top and
is clipped/exposed by the changing local height; a delayed response for an old
keyboard height cannot displace the displayed grid. Resize requests are serialized
and coalesced per attachment. Pointer input is disabled while local/remote cell
geometry differs. Selection still freezes height-driven reflow while reading;
width and font changes invalidate its coordinates.

Read-only `UITextInput` and `UITextInteraction(.nonEditable)` provide iOS's own
selection gestures, highlight, magnifier, handles and edit menu. UIKit UTF-16
positions map to semantic terminal cells, keeping wide and composed glyphs
whole. The existing Rust selection owner pins the displayed source and extracts
clipboard text, preserving soft wraps and hard line breaks. Copy never sends
Ctrl+C. Handle changes are coalesced in order, and a cancelled or superseded
copy cannot overwrite the clipboard. Keyboard dismissal does not reflow a
selection; a width change, reconnect or attachment change clears it.
Logical row positions keep UIKit's anchors stable as older pages arrive. Edge
scrolling requests one row at a time and waits for the resulting frame. The
read-only text document retains the selected span plus the current window,
bounded by 4,096 rows, 65,536 cells and 512 KiB of text. Missing or changed
history and exhausted bounds stop expansion with a visible error; copied text
is never silently truncated. Leaving the screen or backgrounding stops the
edge timer.

The ordered input writer coalesces adjacent plain text under slow transport,
while keeping key, modifier and paste boundaries. Pending input is bounded by
512 KiB and 256 events; overflow is reported visibly. Every queued write retains
its attachment and synchronization epoch, so stale input is discarded.
After a terminal ends or loses control, the status card offers Retry, the Session
panel or confirmed takeover. Automatic process restoration uses the exact saved
host/Session ID and never creates a Session or takes over without confirmation.
Explicit Home clears only the active restoration record; recent/per-host Session
records remain. Explicit host entry and its Retry resolve the live list before
choosing or opening a default Session. Refreshing the panel or renaming a Session
does not retire the current attachment. Late list/connection results cannot
reopen a terminal after the user cancels and returns Home.

Home and terminal chrome use the Android palette and page hierarchy through
SwiftUI. Settings uses a native grouped Form with sections for general,
terminal, notifications, diagnostics, background connection and about. It offers
persisted system/Chinese/English language,
system/dark/light appearance, a 10–20 text-size slider with preview (default 15),
version information, the project link and open-source licenses. Existing saved
appearance settings migrate with language set to system. App-owned labels,
accessibility descriptions and errors use the selected language; system photo
pickers and selection menus retain the device's language.

The Xcode target references the existing Android font and license directories;
it does not duplicate or modify their files. Light, Regular, LightItalic and
Italic map to terminal normal, bold, italic and bold-italic. Core Text advances
preserve one-cell Latin/Nerd glyphs and two-cell Chinese text at every supported
size. Font changes clear an active selection before changing the terminal grid.

Camera QR capture uses AVFoundation on a serial capture queue and stops when
covered, inactive or dismissed. Leaving the foreground keeps a pending multi-code
device choice or image result; an interrupted camera restarts on return.
Denied/restricted/unavailable-camera states keep
image and manual entry available. Photo selection uses `PHPickerViewController`
without requesting photo-library access. The temporary provider file is decoded
to an orientation-corrected image no larger than 2,048 pixels per side, with a
50 MiB input bound. [Core Image's QR detector](https://developer.apple.com/documentation/coreimage/cidetectortypeqrcode)
reads the codes; the current iOS 27 simulator could not initialize the Vision
barcode inference context. Tickets are deduplicated, bounded and inspected by
the native runtime before pairing. Leaving the scanner invalidates pending photo
results. Ticket text and images are never saved by the app or included in logs.

Uploads reserve the native operation before presenting a picker and pause input
until completion/cancellation. The Swift owner retains the exact attachment and
input epoch, rejects duplicate/late picker callbacks, discards queued/marked
input across the pause, and cancels when authority changes or the user leaves.
Rust owns transport, reconnect rules and final path insertion. Photos use the
picker's current file representation without app-side recompression; its temporary
file is copied before the provider callback returns, as required by
[NSItemProvider](https://developer.apple.com/documentation/foundation/nsitemprovider/loadfilerepresentation(fortypeidentifier:completionhandler:)).
External documents use security-scoped access and coordinated reads following
[UIDocumentPickerViewController](https://developer.apple.com/documentation/uikit/uidocumentpickerviewcontroller).
Staging copies in 64 KiB chunks to a private temporary directory, enforces the
shared byte limit while reading and cleans up on completion/cancellation. Failed
transfers retain the bounded copy for an explicit retry; closing removes it.
Only the safe extension is sent to the host, not the original filename. An
unconfirmed remote result explains that retry creates another file. Uploads do
not add a background execution entitlement or promise lock-screen continuity.

The app does not request a background execution mode. iOS may suspend its
networking after backgrounding or locking. Foregrounding refreshes DNS/network
state and reuses the attachment's existing recovery logic. Background continuity,
real keyboard animation, rotation and Wi-Fi/cellular transitions need phone
acceptance; simulator tests do not establish these behaviors.

## Ownership and storage

SwiftUI owns navigation; UIKit owns the terminal canvas, measured size and input.
The app declares a single window scene: one application model owns the attachment,
viewport and input, so iPad multiwindow is disabled rather than letting windows
share and resize the same terminal. The shared `voytty-client` and UniFFI bridge own protocol, transport, synchronization,
history and terminal input encoding. No shell, PTY or ANSI parser runs on iOS.

For this initial slice, both mobile apps use `crates/android` (its historical
package name), without copying its protocol/reducer into a second crate. iOS
generates the **VoyttyNative** Swift API and links the same library statically;
Android keeps its existing Kotlin API and dynamic library. The target-specific
controller label identifies iOS to the host. Android update APIs in the shared
bridge are not used by the iOS UI.

The controller seed, saved hosts, preferences, recent connection and exact active
Session record share one atomic Keychain item, accessible
while unlocked and restricted to this device. Pairing saves the authenticated
host before committing the connection. Corrupt or inaccessible existing state
fails visibly instead of silently replacing the identity. When a Session ends while
the device is locked, clearing its restoration record waits for the next
activation instead of reporting a Keychain failure. Keychain data may
survive uninstall/reinstall; debug and release remain separate. This follows
[Apple's Keychain accessibility guidance](https://developer.apple.com/documentation/security/ksecattraccessiblewhenunlockedthisdeviceonly)
and [application identity/access group rules](https://developer.apple.com/documentation/security/sharing-access-to-keychain-items-among-a-collection-of-apps).

The platform DNS adapter reads the current system resolver through public
`libresolv` APIs, including IPv6 scope IDs. `NWPathMonitor` updates the existing
endpoint after network changes. No private Apple APIs or hard-coded platform
DNS addresses are introduced.

## Verification

`just ios-check <uuid>` runs bridge lifecycle/cancellation, ticket validation,
atomic identity/host persistence, corrupt-state rejection, committed/marked
Unicode input, disconnect admission and DNS shape tests on the chosen simulator.
`auto` selects an available iPhone simulator for CI. `CI gate` requires both the
unsigned device build and simulator checks for full CI; no iOS binaries or
compiled caches are uploaded.

The separate remote acceptance test requires a disposable host ticket. Without
that explicit fixture only this external-network test reports a skip. It pairs,
creates a temporary Session, sends a command whose echoed input does not contain
the expected output, checks Chinese output, resizes, detaches, reattaches to the
same Session ID, extends a selection over more than a screen of history, checks
exact Unicode/soft-wrap copy and return to live output, uploads a 2,000,013-byte
file through the production Swift staging/operation owner, verifies its SHA-256
on the host after an explicit Enter, and finally closes the temporary Session.
It also checks cancelled reservations, paused input and duplicate selection.
Build/run the ordinary simulator checks before issuing the short-lived ticket,
so a first cold build does not consume its lifetime.

```sh
cargo build --locked -p voytty-cli --example presentation_fixture
# Supply a new, short /tmp path; never use ~/.voytty or an existing host.
target/debug/examples/presentation_fixture init /tmp/ios-acceptance-host
target/debug/examples/presentation_fixture start /tmp/ios-acceptance-host
# In another terminal, after the daemon is ready:
target/debug/examples/presentation_fixture pair /tmp/ios-acceptance-host
python3 tools/ios/build.py check --simulator <uuid> \
  --ticket-file /tmp/ios-acceptance-host/pairing-fixture.txt
target/debug/examples/presentation_fixture cli /tmp/ios-acceptance-host daemon stop -y
```

The short-lived ticket enters only the test bundle, never the application target
or Git. The generated source copy is cleared after testing; disposable test
bundles/derived data should be removed when the acceptance run is finished.

For native UI keyboard/notification/export acceptance on Xcode 27, first pair that disposable host
in the simulator app and create a Session to restore afterward. Write its exact
IDs to a private JSON file, then run the opt-in UI recipe:

```json
{"host":"DISPOSABLE_HOST_DEVICE_ID","restoreSession":"EXISTING_FIXTURE_SESSION_ID","fixtureRoot":"/tmp/ios-acceptance-host","daemonPID":"DISPOSABLE_DAEMON_PID"}
```

```bash
just ios-ui-check <simulator-uuid> /tmp/ios-ui-fixture.json
# Target the notification/export UI flow during service edits:
just ios-ui-services-check <simulator-uuid> /tmp/ios-ui-fixture.json
just ios-ui-files-check <simulator-uuid> /tmp/ios-ui-fixture.json
just ios-ui-photos-check <simulator-uuid> /tmp/ios-ui-fixture.json
just ios-ui-recovery-check <simulator-uuid> /tmp/ios-ui-fixture.json
just ios-ui-reconnect-check <simulator-uuid> /tmp/ios-ui-fixture.json
just ios-ui-connection-check <simulator-uuid> /tmp/ios-ui-fixture.json
just ios-ui-navigation-check <simulator-uuid> /tmp/ios-ui-fixture.json
just ios-ui-deletion-check <simulator-uuid> /tmp/ios-ui-fixture.json
just ios-ui-sessions-check <simulator-uuid> /tmp/ios-ui-fixture.json
just ios-ui-settings-check <simulator-uuid> /tmp/ios-ui-fixture.json
```

The UI tests create their own `/tmp` Sessions, check bytes received by a raw remote
PTY and native notification/export interaction, delete those Sessions and return
to the requested existing Session. Notification acceptance authorizes alerts on
the disposable simulator install. The
recipe sends special-key USB HID usages through Xcode 27's DTUHID simulator
service when the test requests them. This avoids XCUITest's observed AppKit-key
translation errors, including F12 arriving as F11. This host-only helper uses
private simulator services and fails explicitly when they are unavailable; it
is never linked into the app. Run this scheme through the recipe, not Xcode's
Test button. Regular `ios-check` uses public XCTest and needs no HID helper.

Live-camera and file-provider phone acceptance, terminal gesture/geometry parity,
locked-phone notifications and iOS distribution/background behavior remain
follow-up work in the Android parity checklist. This slice does not establish
full feature parity.

### Local evidence — 2026-10-22

- Xcode 27.1: simulator app and unsigned arm64 device app build successfully,
  including a cold build in the isolated `target/ios-native` directory.
- iPhone 18 Pro / iOS 27.0: all nine local tests pass. The explicit disposable-host
  run passes all ten tests, including real-Iroh pairing/input/resize/reattach.
  Home and the real terminal's Chinese output were visually inspected.
- Targeted mobile Rust Clippy and 23 tests pass; `just android-check` passes.
  The complete `just check` gate passes: portable policy, workspace Clippy/tests,
  documentation, 441 noq tests, 33 patched Iroh tests, 45 Askama tests, dependency
  policy and relay static checks. These results are local macOS evidence.
- The CLI terminal-identity timeout reproduced during the full run. Bounded
  failure diagnostics showed probe input preceding the first Zsh prompt. The
  test now waits for the shell's line-editor signal, with a default Bash prompt
  path for macOS CI's older Bash. A real Bash fixture with bracketed paste
  disabled covers that path. The ten-second deadline and notification/body
  assertions remain unchanged; no automatic test retries or probe replays were added.
- Dependency policy passes after upgrading `rustls-webpki` to 0.103.16 and both
  `webpki-root-certs` and `webpki-roots` to 1.0.10 in the applicable lockfiles.
  The [Askama upstream backport](../vendor/askama_derive/VOYTTY.md) removes
  `basic-toml`. No advisory exceptions were added. Android, device compilation
  and both simulator test profiles were rerun with these dependencies.

The subsequent foreground acceptance on the same simulator verified pairing and
host persistence across app termination, creating and switching between two
independent shells, Pinyin candidate commit (`你好-iOS`), Enter, Ctrl+C,
arrow/backspace editing, raw Tab (`09`) and Escape (`1b`), and both menu and
Command-V paste. Scroll-wheel input initially did not reach the custom pan
gesture; enabling all UIKit scroll types fixed visible history navigation.

A burst of keystrokes on a high-latency Relay path exposed silent truncation at
the original 32-task input limit. The coalescing writer fixes this: three new
tests cover a suspended writer with 300+ typed characters, ordered edits,
cancel/restart isolation and resource bounds. They are included in the nine
local tests above; the optional remote test needs a fresh ticket. A foreground
300-character command also returns the exact length.

Pausing only the disposable host process for 50 seconds produced the visible
reconnecting state with terminal keys disabled. Resuming it restored the
attachment automatically; the original shell PID and a shell-local marker were
unchanged. This exercises transport interruption, not phone network switching.
Returning from the simulator Home screen also retained the attachment and
accepted another command; real-device suspension remains a separate check.

After the dependency regression, the updated app reopened the saved host and
the original Session. A foreground command returned the same shell PID and
shell-local marker. The one-use pairing fixture was removed; generated and
installed test resources were rebuilt without the ticket.

No phone signing/install, phone IME/network/background acceptance or hosted iOS
CI result is claimed by this local work.

The native-selection follow-up passes all 13 simulator tests with a fresh
disposable ticket and the unsigned device build. Three selection tests cover
UTF-16/whole-cluster mapping, reverse/multiline geometry, hard and soft wraps,
read-only native interaction and frozen selection text. Remote acceptance now
copies Chinese, a composed emoji and a combining character through the shared
selection owner, and checks that cancelling an outstanding copy preserves the
clipboard. The final run completes without reading the pre-existing clipboard
or requesting paste permission.

Foreground long-press selection, system handles/menu and the system Copy action
were observed; Copy returned the selected word exactly. The user subsequently
confirmed that manually dragging the native selection handle expands the range
normally in the foreground simulator. Automated dragging remains inconclusive
because the same automation also fails to swipe the simulator Home screen; the
handle-drag acceptance is manual evidence. That check predates the cross-screen
selection follow-up below.

The cross-screen follow-up passes all 18 simulator tests with a fresh disposable
ticket and the unsigned device build. Tests cover stable logical positions,
offscreen anchors, soft-wrap exclusive endpoints, expansion in both directions,
finger-lift cancellation and bounded history merges. The real-host acceptance
holds the selection at the upper edge through the same timer and frame updates,
scrolls 25 rows in a 20-row viewport, compares the exact copied Unicode/soft-wrap
text, then verifies return to live output. Its bounded wait polls the current
frame because generated UniFFI Swift futures do not observe Task cancellation;
a timeout task group around `waitForFrame` otherwise cannot finish.

Foreground wheel scrolling showed the native **回到最新输出** button, and tapping
it restored the latest output and hid the button. Long-press still showed the
system handles and Copy menu; Copy returned `native` exactly. The user then
confirmed that holding the upper native handle at the edge scrolls and expands
the selection, and lifting the finger stops it. Automated handle dragging
remains inconclusive on this simulator; this gesture result is manual evidence.
Test ticket resources were cleared and the app was reinstalled without a ticket
before this foreground check.

The first Android-parity UI/workflow implementation adds Home/recent cards,
the bottom Session panel, create-with-directory, rename/delete, exact-ID
restoration and functional appearance/text-size settings. The simulator passes
all 25 tests with a fresh disposable-host ticket; the unsigned device build and
`just ci-policy` also pass.
Seven workflow tests cover migration, entry policy, restoration without takeover
or creation, failed/stale lists, cancelled late attachments and create/attach
failure recovery. The opt-in real-host test also exercises directory, rename,
switch, recent entry, restoration and deletion. The UI connection recovered,
allowing foreground inspection of light/dark Home and Settings, persisted font
size, the Session panel, native create/rename forms and deletion confirmation,
switching, and recent entry. A command in a Session created with directory
`/tmp` returns `/private/tmp`. Native long-press selection and system Copy still
return the selected word `native` exactly. Test tickets were cleared and rebuilt
before installation; temporary test Sessions were cleaned up while retaining
the two existing fixture Sessions. No signed-phone acceptance is claimed.

The pairing/preferences follow-up passes all 31 simulator tests with a fresh
disposable-host ticket, plus the unsigned device build and `just ci-policy`.
Six added tests cover appearance-settings migration, language fallback and
translation resources, ticket bounds/deduplication/validation, stale image
callbacks, multiple/no/invalid QR images, and four font faces with CJK/Nerd glyph
ownership and matching cell metrics at every supported size. Remote acceptance
decodes the actual ticket through the image detector before pairing. Initial
shell commands now wait for the fixture account's Bash/Zsh prompt, within the
existing bounded deadline, rather than only transport input admission.

Foreground inspection verifies English/Chinese screens and language persistence
after relaunch, system photo selection
through successful pairing and reconnection to the original Session, denied
camera permission with image/manual alternatives, no-QR and invalid-ticket
errors, license navigation, and real bold/italic/CJK/Nerd output. Native Copy
returns `Regular` exactly with the bundled font. Live camera capture, camera
interruption/rotation and permission changes on a phone remain unverified.

The upload stage adds eight tests (39 total with the explicit disposable host).
The real-host run verifies an exact 2,000,013-byte upload through Swift staging,
SHA-256 on the host, duplicate picker-result rejection, cancellation, input pause
and path insertion without Enter. The unsigned device build and policy checks
pass. Foreground document-picker selection remains subject to the automation
limitation recorded in [the parity evidence](ios-parity.md#upload-acceptance--2026-10-22).

The gesture/geometry stage adds seven tests (46 total with the disposable host).
It covers fractional wheel motion, frame-rate-independent history decay,
captured-frame admission, cell hit testing, obsolete-height retention, deferred
keyboard resizing and ordered pointer/key delivery. The real-host run also
checks history coasting, mouse press/release coordinates and wheel reports,
alternate-screen cursor keys, stale pointer rejection across resize, and native
Copy. Foreground inspection caught a UIKit single-tap focus conflict that the
direct-method tests did not cover; the corrected gesture priority is verified
with actual simulator taps, a drag and native long-press Copy.

Software-keyboard expansion/collapse and landscape/portrait transitions were
inspected in the foreground; remote grid dimensions and new click coordinates
were checked after layout settled. The simulator's automation still delivers
some drag/wheel gestures inconsistently, so continuous touch feel, trackpad
behavior, full hardware-key support and signed-phone keyboard/rotation/IME
acceptance remain separate work. See the
[gesture acceptance record](ios-parity.md#gesture-and-geometry-acceptance--2026-10-22).

The hardware/IME stage passes all 53 XCTest tests with a disposable-host ticket,
plus the opt-in native UI keyboard test and unsigned device compilation. The UI
test checks actual legacy/enhanced bytes, keyboard expansion/collapse and focus
after creating a Session. It also deletes its Session and restores the existing
one. Simulator runtime checks can run while the Mac is locked. Phone IME,
language switching and sustained physical-key repeat remain separate acceptance;
see the [keyboard evidence](ios-parity.md#keyboard-and-ime-acceptance--2026-10-22).


## Notifications and diagnostics

Settings → Terminal notifications requests system authorization only when the
user enables the switch. Launch never prompts. Foreground notifications use
native banners; tapping a notification opens the existing app without trusting
notification content as a command or connection target. The app preference and
current system authorization both gate delivery. Native generations and the
application attachment epoch are checked immediately before posting. Switching
Sessions, turning notifications off or changing OS permission cannot queue old
events for later delivery. Permission denial provides a link to iOS notification
settings, and authorization is refreshed on activation.

Local notifications are delivered only for OSC 9/777 events the live connection
receives. iOS may suspend background execution; this version does not promise
background delivery after suspension and does not replay missed events. Android's
foreground service has no direct equivalent here. See Apple's
[notification authorization](https://developer.apple.com/documentation/usernotifications/asking-permission-to-use-notifications)
and [background strategies](https://developer.apple.com/documentation/backgroundtasks/choosing-background-strategies-for-your-app).
Signed-phone lock/unlock, suspension and network-transition acceptance remain
separate from simulator checks.

Settings → Diagnostics remains available if identity/network initialization
fails. The shared Rust recorder owns typed event admission, safe fields, timed
detail and bounded queues. Swift owns private 0700/0600 files excluded from
backup, separate 2 MiB current/archive budgets for key and detailed events,
one 15-minute recording switch with renewal and a UIKit document exporter that
includes saved detail even after recording stops. The sink
uses verified directory/file descriptors, rejects symlinks and hard-linked or
nonprivate files, and never re-enters Rust from a callback. Export captures at
most 8 MiB under the storage lock, then validates records off the main thread;
unknown/partial records are omitted and counted in the header. Export staging is
removed after the system sheet fully dismisses and on the next application start
after an interruption. Admission stays closed during dismissal so an old callback
cannot remove a new export. Nothing is uploaded automatically. See [logging](logging.md).


Data Protection is explicitly requested for each writable file and atomic
replacement using `completeUntilFirstUserAuthentication`; a protection API
failure prevents payload writes. This iOS 27 simulator omits the protection
attribute even after a successful explicit request. Simulator tests verify
private modes, backup exclusion, protected-write failure handling and this
platform boundary; the same test retains an exact protection-class assertion
when compiled for a phone. Encryption and locked-phone access require physical
hardware and are not established by these simulator results.

## iOS lifecycle and distribution boundary

Foreground activation refreshes DNS and Iroh network paths on the existing
endpoint. Visibility messages are ordered, newly attached terminals inherit the
current visibility, and backgrounding drops queued input and modifiers. Remote
processes remain on the host; restoration retains the exact Session identity.

An owned terminal connection requests finite UIKit background time on entering
the background, so a short app switch can continue using the existing connection.
Inactive transitions such as system prompts do not start a request. Returning
to active releases it; explicit detach, an ended Session or loss of control also
releases it. A background visit makes at most one request, including when iOS
denies it or it expires. Frame updates and reconnect attempts cannot renew it.
Expiration ends the assertion without detaching the terminal or replacing the
Session. The existing transport handles any actual disconnect when execution
resumes. No fixed background duration or continuously connected suspended app is
promised. Notifications may arrive while the live connection has execution time;
this does not implement APNs delivery after suspension.

Uploads request a finite UIKit background assertion when source preparation
starts. Completion, cancellation and failure release it; expiration cancels the
current upload and presents an explicit foreground retry. An old expiration
cannot cancel a newer attempt. This follows Apple's
[background execution contract](https://developer.apple.com/documentation/uikit/extending-your-app-s-background-execution-time).
The connection and upload own separate assertions, so ending one cannot cancel
the other. Their combined runtime still belongs to iOS; multiple assertions do
not create extra time. Settings describes connection retention during short app
switches and recovery after suspension, separately from Android's foreground
service. No repeated renewal or unrelated background mode is used.

The iOS app is currently a development installation. A signed phone build needs
an Apple development team and an unlocked connected device. App Store/TestFlight
identity, signing, review and publication are not configured by this feature
branch. Android's authenticated APK installer cannot install iOS updates; iOS
distribution must use the chosen Apple channel. No nonfunctional update action
is shown and the existing released-platform pipeline is unchanged.

UI acceptance now requires the disposable host root as well as host/restore
Session IDs. The recipe validates its marker and only takes over or cleans up
random Sessions created by this run. File checks create two uniquely named
Files-provider documents and remove them afterward. Photo checks grant the
**test runner**, not voytty, Photos access to create/delete one test asset; the
production picker still requires no broad photo-library permission. All iOS
recipes hold a build lock to prevent overlapping builds from replacing the
generated test resources.

The reconnect check verifies the PID still names that exact fixture daemon,
pauses it until the transport times out, then resumes it when the UI observes
reconnection. A 60-second failsafe and the runner's cleanup resume it if the
test fails. The host shell is preserved. Photo fixture identities are journaled
in the test runner's Documents directory until native Photos cleanup succeeds,
so an interrupted test can clean up its own asset on the next run.

# iOS / Android parity

## Target agreed on 2026-10-22

First deliver the current Android app's interface and functions on iOS using
Swift. Match page structure, navigation, controls, visual hierarchy and behavior;
do not replace that target with a separate iOS MVP roadmap. SwiftUI/UIKit own the
iOS application and UI. Reuse the existing Rust protocol, transport and terminal
state through UniFFI, as Android already does.

Selection gestures, selection handles, the Copy menu, keyboard/IME and system
pickers use iOS-native implementations. Preserve the existing verified native
selection work. Interactive simulator review stays visible in the foreground;
command-line/XCTest/XCUITest checks may run while the Mac is locked. Real-phone
signing and acceptance remain pending device availability.

## Baseline

Use the Android source in this checkout as the reference. Documentation can lag
the UI: for example, the current toolbar has a Session chip, image/file uploads,
Esc, Ctrl, four arrows and the keyboard toggle; Tab/Alt are no longer standalone
buttons in that row.

- [Screens, home and settings](../apps/android/app/src/main/java/io/github/leonfox28/voytty/AppUi.kt)
- [Theme, colors and typography](../apps/android/app/src/main/java/io/github/leonfox28/voytty/Theme.kt)
- [Terminal layout and Session panel](../apps/android/app/src/main/java/io/github/leonfox28/voytty/TerminalScreen.kt)
- [Navigation and connection behavior](../apps/android/app/src/main/java/io/github/leonfox28/voytty/AppRepository.kt)
- [Persisted state](../apps/android/app/src/main/java/io/github/leonfox28/voytty/AppStore.kt)
- [Android behavior and acceptance boundaries](android.md)

## Gap inventory

Status describes the current iOS implementation, not planned completion.

| Area | Android reference | Current iOS gap |
| --- | --- | --- |
| Home | Brand header, Settings, recent connection card, saved-host monograms/cards, empty state, floating Add host button | Brand/header, cards, recent connection, Settings and floating add entry implemented; populated light/dark screens inspected in the foreground |
| Navigation | Home / Scanner / Settings / Terminal; host entry selects the remembered/available Session | All four routes and host-entry policy implemented |
| Terminal layout | Grid fills the screen above the bottom toolbar; Session chip opens a panel; route/latency and machine details in the panel | Host, route and latency appear in the native navigation bar; bottom Session chip opens a native medium/large sheet with a grouped Session list; image/file upload buttons and status/recovery UI implemented |
| Session management | Create with optional directory, switch, rename, confirm deletion, confirm takeover, refresh without retiring attachment | Create/directory/switch/rename/delete and attachment-preserving refresh pass real-host tests; create/rename/switch and native deletion confirmation inspected in the foreground; native occupied-session confirmation/cancellation, confirmed takeover and recovery after shell exit pass real-host UI acceptance |
| Saved/restored state | Per-host last Session, recent connection, exact host/Session process restoration; explicit Home clears active restoration | Backward-compatible last/recent/active records and exact-ID restoration pass runtime tests; recent entry also verified in the foreground |
| Pairing | Camera QR, image QR, manual ticket; device choice for multiple valid tickets; open host after saving | Camera/image/manual routes, multi-ticket choice and host entry implemented; real-ticket image decoding passes runtime acceptance; live camera requires phone acceptance |
| Terminal input | System IME, shortcuts, ordered input, paste, pointer-aware taps/swipes and alternate scrolling | IME, ordered input/paste, upload pause, mouse/alternate-scroll routing and deferred geometry implemented; native Pinyin preedit/candidate commit passes foreground byte verification; light/dark keyboard and landscape inspected; hardware shortcuts, hidden-keyboard focus and enhanced key release pass native UI/remote checks; phone IME and sustained physical-key behavior remain pending |
| History and selection | Swipe/fling, cached history, native Copy, captured selection spanning pages | History, row-based touch inertia and iOS-native cross-screen selection implemented; touch tuning and full phone gesture comparison remain pending |
| Font/rendering | Bundled LXGW Bright Code NF GB faces, Chinese/Nerd glyphs, real bold/italic, host cell geometry and colors | Same four font files bundled; CJK/Nerd ownership, style metrics and cell advances pass tests at sizes 10–20; hidden/struck text, five colored underline styles, wide-cell backgrounds/cursors and cursor blink have raster/lifecycle regression coverage |
| Preferences | System/Chinese/English, system/dark/light, size 10–20 with preview and persistence | Native grouped Form contains language, theme and size controls with compatible persistence and bundled-font preview |
| Uploads | Native image/file pickers, 50 MB limit, progress/speed, cancel/retry, insert returned path once without Enter | Native photo/document pickers, bounded staging, host-accepted progress/speed, cancel/retry and native path insertion implemented; real native document/photo selections pass remote SHA-256 verification without automatic Enter, including oversized-file rejection and native cancellation; cloud providers and OS background deadlines require phone acceptance |
| Connection states | Connecting/cancel, reconnect, ended/occupied/lost control, retry/picker/confirmed takeover | System alerts for failure/occupancy/control loss/Session end, native unavailable state, cancel, retry, picker and confirmed takeover implemented; entry/restore distinctions pass workflow tests; workflow tests cover missing/ended/unauthorized/transport/occupied failures; native UI checks cover lost control, takeover, shell exit and actual transport interruption/reconnect without replacing the Session |
| Notifications | Opt-in controls, OSC 9/777 notification delivery, no stale replay | Native authorization/settings, OSC 9/777 foreground banners and tap-to-return verified; app/OS gates and attachment generations discard stale events; suspended-background delivery requires a separate iOS strategy |
| Diagnostics | Bounded local logs, timed detailed logging, export through system picker, available after initialization failure | Private bounded logs, one 15-minute recording switch with renewal, automatic inclusion of saved detail in exports and shared safe filtering implemented; native document save/cancel/continuous export verified; independent of identity/network initialization; physical Data Protection remains phone acceptance |
| About/update | Version, project link, verified Android APK update flow | Version, project link and licenses implemented; device installation uses Xcode signing, distribution/updates use an authorized Apple channel (TestFlight/App Store), with no APK installer equivalent; no channel is configured or published |
| Background lifecycle | Android foreground-service preference and status notification; process restoration independent of it | Ordered foreground/background visibility, stale-input cancellation, exact Session restoration, finite connection retention during app switches and separate upload background time implemented; one connection-time request per background visit, released on foreground/Session end; expiration retains the terminal for transport recovery; the notification setting explains suspension/delivery limits; OS scheduling and phone network transitions remain external acceptance |

## Delivery order

1. **Match the application structure and Session workflow.** Port the theme and
   shared visual components; align Home, empty state, host cards and recent card.
   Align the terminal's bottom toolbar and in-terminal Session panel. Implement
   the associated host-entry policy, Session create/directory/switch/rename/delete,
   refresh and recovery controls, with persistence for last/recent/active state.
   Keep the working terminal and native selection underneath this UI.
2. **Complete pairing and preferences.** Camera/image/manual pairing, language,
   light/dark/system theme, font-size control and the same licensed terminal font
   faces. Verify light/dark and Chinese/English on the same screens.
3. **Complete terminal operations.** Image/file uploads and their lifecycle,
   pointer/alternate-screen gestures, kinetic history, hardware/IME input and
   keyboard/rotation geometry. Verify controls against actual remote behavior.
4. **Complete application services and platform differences.** Notifications,
   diagnostics/export, About, restoration/background behavior and the iOS update
   path. Android APK installation and foreground-service mechanisms require
   explicit iOS equivalents; do not add nonfunctional switches or claim parity
   by copying labels. Distribution publication is a separate release action.
5. **Run the parity acceptance matrix.** Compare each route and state against
   Android, including failures, permission denial, empty lists, occupied Sessions,
   reconnect, cancellation and persistence. Record simulator evidence separately
   from signed-phone, camera, network and background evidence.

An interface-only screen with inactive controls is not a completed feature.
Keep unsupported platform behavior visible in this checklist until the iOS
equivalent and its acceptance evidence are recorded.

## First workflow acceptance — 2026-10-22

The iPhone 18 Pro simulator on iOS 27.0 passes all 25 tests with a fresh ticket
for a disposable host. This includes seven new workflow tests plus real-host
creation with directory, rename, switch, recent entry, exact-ID restoration,
deletion and the existing input/native-selection checks. The unsigned device
build and `just ci-policy` also pass. Ticket resources were cleared and rebuilt
before installing the final app.

Foreground inspection covers light/dark Home and Settings, size preview and
persistence, the bottom Session panel, native create/rename forms, the deletion
confirmation, switching and recent entry. A command in the newly created
`/tmp` Session returns `/private/tmp`. System Copy still returns the selected
word `native` exactly. The temporary UI Session was cleaned up, preserving the
two existing fixture Sessions. Automated tests finished before leaving the app
open for inspection. These results do not complete the remaining rows above or
replace signed-phone and side-by-side Android acceptance.

## Pairing and preferences acceptance — 2026-10-22

All 31 simulator tests pass with a disposable ticket, including actual QR
decoding before pairing, multi-code validation, stale image completion,
settings migration/localization, font metrics and the existing Session/input/
selection regression. The unsigned device build and policy checks pass.

Foreground evidence covers language switching and persistence after relaunch,
native photo selection followed
by pairing and restoration of the original Session, denied camera access,
no-QR/invalid-ticket errors, the manual entry form, license navigation and all
four font styles with Chinese/Nerd glyphs. Native selection/Copy remains intact.
The app uses AVFoundation for live capture, Core Image for image decoding and
the system photo picker. Live camera, camera interruptions/rotation and phone
permission transitions still require signed-device acceptance.

## Upload acceptance — 2026-10-22

All 39 simulator tests pass with a fresh disposable-host ticket. Eight upload
regression tests cover bounded/exact staging and cleanup, empty files, cancelled
reservations, stale picker/epoch results, duplicate delivery, retained-copy retry,
photo-provider temporary-file lifetime and marked text during an input pause.
The real-host test uploads 2,000,013 bytes through the production Swift owner,
checks SHA-256 after an explicit Enter, and verifies cancellation, input pause,
unchanged input epoch and no automatic Enter. The unsigned device build and
policy checks pass; no shared Rust or Android sources changed in this stage.

Foreground inspection verifies the toolbar and the native document picker.
The automation service currently returns `noWindowsAvailable` when clicking
inside the document provider's system panel; manual file-selection confirmation
is pending. This limitation does not establish end-to-end picker UI acceptance,
cloud-provider availability or signed-phone/background behavior. Test ticket
resources were cleared and rebuilt before installing the final app.

## Gesture and geometry acceptance — 2026-10-22

All 46 simulator tests pass with a disposable-host ticket after the foreground
gesture-priority fix. Seven added tests cover fractional/bounded wheel motion,
time-based history decay, stale geometry/authority admission, cell hit testing,
old-height retention, keyboard transition deferral and ordered pointer delivery.
The real-host test checks actual mouse press/release coordinates and wheel
reports, alternate-screen cursor keys, rejection of queued/held stale clicks
after resize, history coasting, and native Copy alongside existing workflows.

Foreground taps initially exposed UIKit's single-tap focus action intercepting
remote mouse input. The corrected public failure relationships preserve native
long press/multi-tap/handle priority. Actual simulator taps now yield matching
press/release counts and coordinates, a vertical drag produces wheel reports,
and long-press system Copy returns `TOUCH` exactly without generating a click.
Keyboard expansion/collapse and rotation preserve the top-anchored grid and
toolbar; the host reports the settled landscape keyboard viewport as 100 × 8,
and subsequent clicks use the new coordinates. Returning to portrait restores
the original dimensions. Some injected drag/wheel attempts remain inconsistent,
so this evidence does not establish full touch/trackpad feel or replace phone
acceptance. Complete hardware-key handling and phone IME remain pending.

The temporary UI Session was closed and the original two fixture Sessions
preserved. The device build is unsigned; no signed-phone acceptance is claimed.
The unsigned device build and `just ci-policy` pass. Test ticket resources are
cleared and rebuilt before final installation. No shared Rust or Android source
changes were needed for this stage.

## Keyboard and IME acceptance — 2026-10-22

Swift changes add Control/Option shortcuts, navigation/F1–F12 keys, captured
press/repeat/release events, focus while the software keyboard is hidden, and
composition-aware routing. Ordinary text, dead keys, active marked text,
Command shortcuts and Control-Space language switching remain with UIKit.
Preedit uses UIKit's marked-text display at the terminal cursor. Session forms,
panels and pickers suspend terminal focus. Hardware events share the ordered
writer and cannot replay across an input pause or authority change.

Seven regression tests and a real-host legacy/enhanced-keyboard byte comparison
have been added. All 53 XCTest tests pass with the disposable host, and the
unsigned device build passes. The seven tests cover navigation/modifiers,
press/repeat/release, stale epochs/input pauses, rejected presses, native marked
text and hidden-keyboard focus. The real-host path compares exact legacy bytes,
Chinese/emoji/combining text, bracketed paste and enhanced keyboard events.
The history-selection fixture waits for its final shell prompt before freezing
the selection, within the existing deadline, so late output cannot move the
edge-scroll baseline. No test retries or weaker byte/selection assertions were
introduced.

The opt-in XCUITest also passes without a desktop window under the user's
authorization. It creates a Session through the UI, types with the keyboard
hidden, checks Control-C, Option-B, Shift-Tab, Home/End/Delete/PageUp/PageDown,
F1/F12, Up/Enter/Backspace, then types after showing and hiding the keyboard.
It compares the exact remote bytes and enhanced Control-C press/release reports.
It deletes its temporary Session and restores the original one. The test exposed
a sheet-dismissal focus race, now fixed by waiting for the editor's dismissal
callback before restoring terminal focus. Child accessibility IDs also remain
distinct rather than inheriting a panel identifier.

Xcode 27.1's public `typeKey` dropped several special keys and sent F11 for F12.
The UI recipe therefore sends those physical USB HID usages through the
simulator's DTUHID service. Temporary UIKit event logs confirmed the correct
codes and both down/up events; these logs were removed afterward. The helper
runs only on the Mac, uses private simulator services and is not shipped in
the Swift app. No product key mapping or expected bytes were changed to
accommodate the faulty test synthesis.

The earlier foreground Pinyin/system-paste/native-selection checks remain
separate evidence. This stage exercises UIKit marked text programmatically;
it does not claim a new candidate-bar or signed-phone IME acceptance. Sustained
physical-key repeat and phone language switching remain to be checked.
Ordinary printable keys use UIKit committed text rather than invented physical
press/release reports; full physical-key reporting for enhanced terminal
protocols is not claimed.

## Acceptance policy

- Compare appearance and navigation in the foreground, using matching theme,
  language, text size and fixture data. Preserve iOS safe areas and native controls.
- Exercise real paired-host behavior for Session management, upload, reconnect,
  notifications and history. Never use the user's real daemon for mutation tests.
- Use the shared `just ios-device-build` and `just ios-check <uuid>` recipes.
  Shared Rust/UniFFI changes also require affected Rust tests/Clippy and
  `just android-check`; run `just check` before pushing product changes.
- Keep existing native-selection acceptance intact: Unicode/soft-wrap copy,
  stable cross-screen anchors, edge auto-scroll and stopping when the finger lifts.
- Do not mark Android parity complete while required rows above remain missing,
  or substitute simulator results for phone-only behavior.


## Notifications and diagnostics acceptance — 2026-10-23

All 63 XCTest tests pass with a fresh disposable-host ticket. Ten added tests
cover permission admission and change races, app preference migration, stale
attachment rejection, posting-failure continuity, storage modes/backup exclusion,
separate retention lanes, unsafe links, protected-write failure, detail expiry,
shared export filtering and export dismissal ownership. Real-host acceptance
emits OSC 9 and OSC 777 with Unicode title/body, reattaches the same Session and
checks that only a fresh event is delivered. Existing keyboard, upload, native
selection, gesture and Session workflows still pass.

The final full UI scheme passes both tests (hardware keyboard and application
services). Native UI service acceptance grants system permission from Settings, enables
and restores timed detail, includes saved detail, saves a uniquely named JSONL
through the system document picker, immediately exports again and cancels,
receives an actual iOS foreground banner, then taps it to return. The saved
file's JSONL header/records were inspected before cleanup. UIKit owns filename,
destination, save and cancel controls. A race found by this test is fixed:
export admission stays closed until the previous system sheet finishes
closing, so its late callback cannot delete a new staging file. Screenshots
record the real exporter and notification banner, without requiring desktop
window access. Temporary Sessions are deleted and the original is restored.

The unsigned arm64 device build and `just ci-policy` pass. The iOS 27 simulator
does not expose Data Protection attributes, including after explicit requests;
its checks are not evidence of device encryption or locked-file accessibility.
The test retains exact protection-class assertions on phone builds and verifies
that protection API failures prevent payload writes on both platforms. Signed
phone lock/unlock, OS notification settings changes, file providers and long
background suspension remain separate acceptance. No Android foreground-service
or APK-update control is copied into iOS without an implemented equivalent.


## Rendering, lifecycle and full native UI acceptance — 2026-10-23

The Swift renderer now covers hidden glyphs/decorations, strikethrough on blank
cells, all five colored underline styles, leading-cell ownership of wide
backgrounds, wide block/underline cursors and narrow beam cursors. A 500 ms
cursor blink stops while selecting, hidden, removed from a window or inactive.
The native text edit menu adds Open Link for a source-pinned HTTP(S) selection;
it revalidates the same controller and URL before handing off to iOS. Copy and
selection handles remain UIKit-owned.

Uploads request finite UIKit background execution only while preparing or
transferring a selected source. Expiry cancels that attempt with an explicit
unconfirmed-result/retry message; old expiry and preparation callbacks cannot
revive it or cancel a newer attempt. Application visibility is ordered, inherited
by new attachments and gates input. Background transitions cancel queued input
and pointer/modifier state. Settings explains iOS suspension and notification
limits; no Android foreground-service toggle is represented as an iOS guarantee.

All six native UI tests pass together on iOS 27.0 / iPhone 18 Pro with Xcode 27.1:

- File picker: reject a 50,000,001-byte source, upload a valid source, verify its
  exact SHA-256 on the host, require explicit Enter and cancel the native picker.
- Photo picker: select a generated PNG through PHPicker, verify its exact host
  SHA-256 without automatic Enter, then remove the test-owned Photos asset.
- Hardware keyboard: hidden/visible keyboard routing, exact legacy bytes and
  enhanced shortcut press/release events.
- Application services: native notification permission/banner/tap, timed detail,
  native export save, immediate second export and cancellation.
- Recovery: a real competing CLI takes control; cancel and then confirm native
  takeover, enter output, background/foreground the app and exit the remote shell.
- Reconnect: pause only the explicitly marked disposable daemon, observe the
  real transport timeout and disabled input, resume it and verify old/new output
  in the same Session. A bounded safety timer and final cleanup resume the daemon.

Native picker acceptance exposed three product lifecycle defects, now fixed:
the document picker may clear its binding before delivering the chosen URL;
a fast photo upload may finish before its picker dismisses; SwiftUI may reuse a
completed document-export controller. Delegates now own picker completion,
keyboard focus waits for actual dismissal, and each export has its own controller
identity plus guarded completion/cleanup. The UI checks retain the exact upload,
notification and input assertions. Foreground input waits for readiness, and the
reconnect test locates the observed native ActivityIndicator by its identifier.
No automatic retries or weaker content assertions were added.

The final UI result bundle is
`target/ios/Logs/Test/Test-VoyttyUI-2026.10.23_01-39-12-+0800.xcresult`.
Screenshots include the native pickers/confirmations, ended-session card and both
sides of the real reconnect. The helper cleans only randomly named test Sessions;
the two pre-existing fixture Sessions remain available. Photos access is granted
to the UI-test runner only; the product retains PHPicker's limited-access model.

Remaining external acceptance is a signed iPhone: live camera and interruptions,
IME/physical-key repeat, touch/trackpad feel, cloud file providers, Data Protection
while locked, cellular/Wi-Fi changes and real OS background expiry/suspension.
There is no connected physical device or signing identity on this machine.
Development installation is documented through Xcode signing; TestFlight/App Store
publication and its update channel are separate authorized release work. Simulator
and unsigned builds do not establish those results or complete a simultaneous
side-by-side Android/phone comparison.


Final local gates: all **71 XCTest tests pass with zero skips**, including a fresh
disposable ticket and actual OSC 8 link selection, native copy, upload, lease
handover and lifecycle behavior. The result is
`target/ios/Logs/Test/Test-Voytty-2026.10.23_01-46-43-+0800.xcresult`.
The added picker-dismissal test uses an independent upload object per reservation,
as production does, and retains its original 3-second deadline and assertions.
The unsigned arm64 device build, `just check`, `just android-check` and
`just ci-policy` pass on this Mac. Hosted Linux and release-signing checks remain
with their documented CI owners. Test resources are cleared and rebuilt before
final simulator installation; the real account's daemon is never mutated.

## Foreground native interaction review — 2026-10-23

Foreground review on the same iPhone 18 Pro simulator exposed three UI defects:
a stationary long press at the top edge could scroll and extend the selection;
Pinyin marked text was transparent at the terminal cursor; and the keyboard
was forced dark even with the light theme. Edge scrolling now starts only after
8 points of finger movement and resets on lift. UIKit marked text uses its
dynamic label color, and keyboard appearance follows the app theme. Selection
gestures, handles and Copy remain UIKit-owned.

A new bounded regression covers stationary long press, small touch jitter,
drag admission, stopping on lift and a subsequent independent touch. Foreground
review confirms a top-row word stays selected without scrolling; native Copy
puts exactly `COPY` on the pasteboard. Open Link in the native edit menu opens
the exact OSC 8 URL `https://example.com/terminal` in Safari. Automation still
cannot reliably drag the native handles; this is not a new manual drag pass.
The user's earlier successful handle/edge-scroll acceptance remains separate.

The actual system Pinyin keyboard visibly displays `ni hao` while the remote
raw-input fixture receives no bytes. Selecting the native `你好` candidate sends
exactly its UTF-8 bytes once; explicit Return adds exactly one CR. Light and dark
keyboard appearance and landscape layout were inspected in the foreground.
English input, portrait orientation, system theme and the original Session were
restored; only the newly created review Session was closed. Remote selection
checks keep their test pasteboard content until the run finishes, then the
review harness restores the saved simulator clipboard. Restoring external
pasteboard items between assertions triggered a deferred iOS paste-permission
alert, interrupted the active gesture and failed its bounded edge-scroll wait;
that intermediate cleanup was removed without weakening the assertions.

The final result is
`target/ios/Logs/Test/Test-Voytty-2026.10.23_09-51-34-+0800.xcresult`:
72 tests pass, with zero failures, skips or recorded runtime warnings, including
real-host acceptance. The unsigned arm64 device build and `just ci-policy` pass.
Earlier full UI, Rust and Android gate results above remain separate; this
review changes only Swift and its tests. Test resources are cleared and rebuilt
before the final foreground installation. Signed-phone acceptance remains
outstanding.

## Finite background connection retention — 2026-10-23

The app now requests UIKit background time for an owned terminal when its scene
enters the background. Short app switches retain the existing terminal instead
of relying only on restoration after suspension. Foreground return, explicit
detach, Session end and lease loss release the assertion. Expiration or denial
cannot trigger another request during the same background visit; a late expiry
cannot end a later visit. Expiration never detaches the terminal. Upload and
connection assertions have independent owners while sharing the OS time limit.
Transient inactive states do not start an assertion. Background input remains
blocked and queued input/modifiers are discarded as before.

All 77 XCTest tests pass with zero failures, skips or recorded runtime warnings:
`target/ios/Logs/Test/Test-Voytty-2026.10.23_10-09-54-+0800.xcresult`.
Five added regressions cover lifecycle admission/release, expiration, denial,
stale callbacks, loss of ownership and independence from uploads. The real-host
workflow also checks output arriving during the background phase, unchanged
terminal object and input epoch after return/network refresh, release on actual
lease loss, and recovery after a deterministically injected expiration.

Both targeted native UI checks pass: recovery/app switching
(`Test-VoyttyUI-2026.10.23_10-10-59-+0800.xcresult`) and actual transport interruption
(`Test-VoyttyUI-2026.10.23_10-11-54-+0800.xcresult`). The recovery check leaves the
app in the background while its remote shell produces output, returns, reads
that output and continues input. UIKit's simulator log independently records
creation of `Terminal connection` task 8 at 10:11:34 and explicit release on
foreground return at 10:11:38. This observed short visit is not a promised
background budget. OS expiration/denial are simulated in the deterministic
checks; actual phone scheduling and long suspension remain phone acceptance.

The unsigned arm64 device build and `just ci-policy` pass. Only Swift, tests and
documentation changed in this stage. Test resources are cleared and rebuilt for
the final foreground install, and both pre-existing fixture Sessions are kept.
APNs delivery after suspension is still a separate unimplemented capability.

## Native upload presentations — 2026-10-23

Upload progress now uses a system sheet with a standard navigation bar, Form,
ProgressView and Cancel action. Failed operations use a system alert with Retry
and Close. The hand-drawn dimming overlay and rounded dialog are removed.
Session creation/rename already use a native Form sheet; deletion, takeover and
host removal already use native confirmation dialogs. The terminal's Session
navigation panel remains an in-screen panel.

Presentation ownership lasts through sheet dismissal before restoring terminal
keyboard focus or presenting another surface. UIDocumentPicker can complete its
sheet dismissal before delivering the selected URL: that interval waits for the
native delegate instead of reopening the picker. A progress sheet admitted just
before a fast failure is allowed to appear before it is dismissed, ensuring its
ownership is released by onDismiss. These transitions use lifecycle callbacks,
without fixed delays or automatic retries.

The file UI acceptance now asserts an actual system Alert, retries the rejected
oversized file, closes the second alert, uploads a valid file and verifies its
SHA-256 on the disposable host. It also checks that upload does not send Enter
and that picker cancellation restores input. This passes in
`Test-VoyttyUI-2026.10.23_10-27-51-+0800.xcresult`. The photo picker, exact uploaded
image digest and input restoration pass in
`Test-VoyttyUI-2026.10.23_10-29-27-+0800.xcresult`.

The initial file check exposed the dismissal transition; a subsequent check
confirmed the native alert but required firstMatch for iOS's duplicated button
accessibility nodes. Both issues were corrected before the final passing run.
No test assertion was removed, deadline extended or retry wrapper added.

All 77 XCTest tests, including real-host acceptance, pass with zero failures,
skips or recorded runtime warnings in
`Test-Voytty-2026.10.23_10-30-34-+0800.xcresult`. The unsigned arm64 device build
and `just ci-policy` pass. These changes affect Swift UI, its UI test and docs;
earlier Rust/Android gate results remain separate. Signed-phone acceptance is
still outstanding.

## Native connection and Session alerts — 2026-10-23

Connection failure, occupied attachment, loss of control and Session end now
present system alerts. The custom recovery card is removed. With no terminal
frame, progress and native ContentUnavailableView provide the underlying state;
with an existing frame, dismissing an alert leaves the last output unobscured.
The Session chip remains available to navigate or select another Session.

A failed attempt or terminal observer publishes a notice once. Metadata refresh,
network updates and ongoing automatic reconnect do not re-present dismissed
notices. A new attempt has a new notice identity; stale dismiss callbacks cannot
clear it, and actions validate the host, Session and latest notice identity.
Notices wait while a form, picker or upload presentation owns the UI. Takeover
requires the explicit destructive confirmation and explains loss of control on
the other device. Ended Sessions offer the list instead of Retry creating a
replacement. Chinese and English use the same native presentation.

All 79 XCTest tests pass with zero failures, skips or recorded runtime warnings:
`Test-Voytty-2026.10.23_10-41-42-+0800.xcresult`. Two new regressions cover notice
dismissal, stale callbacks, explicit retry, route changes and suppression during
live/reconnecting states. Existing workflow error cases now assert the correct
notice kind and exact Session; real-host acceptance checks lease loss, occupied
retry and clearing after confirmed takeover.

Native UI acceptance passes for actual control loss, cancellation without
control acquisition, nonrepeating refresh, an occupied connection attempt,
confirmed takeover, short app switching and shell exit
(`Test-VoyttyUI-2026.10.23_10-45-36-+0800.xcresult`). A new shared
`just ios-ui-connection-check` recipe pauses only the marked disposable daemon,
observes the real failed connection's system alert, resumes the daemon and uses
the alert's Retry action to recover the exact Session and its output
(`Test-VoyttyUI-2026.10.23_10-43-41-+0800.xcresult`). Automatic transport reconnect
also passes and asserts that no modal alert interrupts recovery
(`Test-VoyttyUI-2026.10.23_10-46-41-+0800.xcresult`). Screenshots of the native
failure, occupancy, control-loss and ended alerts were visually inspected.

The existing native file picker/oversize-alert/retry/upload flow also passes
(`Test-VoyttyUI-2026.10.23_10-47-58-+0800.xcresult`), verifying coexistence with
the new connection alerts. The unsigned arm64 device build and `just ci-policy`
pass. No Rust/shared Android source changed in this stage. Simulator evidence
and unsigned compilation remain separate from signed-phone acceptance.

## Native navigation — 2026-10-23

Home is now the root of a SwiftUI `NavigationStack`. Terminal, settings and
pairing use the system navigation bar and Back button, including the system
interactive edge swipe. The terminal title follows the current Session (or
host while connecting), and the viewport is laid out below the bar so neither
text nor selection handles need an overlapping floating button. The redundant
Home button inside the Session panel and custom settings/scanner headers are
removed. Existing terminal scrolling and native text gestures are unchanged.

A committed pop calls the existing cancellation/detachment boundary. A cancelled
interactive pop leaves the attachment intact; returning Home clears automatic
terminal restoration while preserving the remembered Session and its remote
processes. The dedicated `just ios-ui-navigation-check` recipe checks button
access without opening the panel, terminal geometry with and without the
software keyboard, cancelled/completed edge swipes, settings/scanner navigation,
the same remote shell after returning, and staying Home after relaunch.

All 79 XCTest tests pass, including real-host acceptance, with no failures,
skips or recorded runtime warnings
(`Test-Voytty-2026.10.23_11-33-45-+0800.xcresult`). The unsigned device build and
`just ci-policy` pass. Screenshots of the terminal with and without its keyboard,
settings and pairing were visually reviewed.

The eight-scenario UI run
(`Test-VoyttyUI-2026.10.23_11-19-40-+0800.xcresult`) passed navigation, hardware
keys, photos, connection failure/retry, takeover/end recovery and transport
reconnect. It exposed two old picker-test selectors matching the app's newly
native Back button behind the modal. The tests now wait for a remembered folder's
file and scope picker Back to `Browse View (Picker)`, retaining the upload
SHA-256, explicit Enter, cancellation and notification assertions. Both affected
recipes pass after the correction: file upload
(`Test-VoyttyUI-2026.10.23_11-32-37-+0800.xcresult`) and diagnostic export/notification
(`Test-VoyttyUI-2026.10.23_11-31-25-+0800.xcresult`). Together these cover all eight
UI scenarios; no automatic retry or assertion relaxation was added.

## System deletion confirmation — 2026-10-23

Each Session row keeps its native action menu. Delete opens a centered system
alert with the Session name in its localized title, the process-termination
explanation, and explicit Cancel and destructive Delete actions. Presentation
is keyed by Session ID and deletion uses the captured Session.

`just ios-ui-deletion-check` passes on iPhone 18 Pro / iOS 27.0
(`Test-VoyttyUI-2026.10.23_11-52-42-+0800.xcresult`). It checks the native Alert,
centered geometry, title, explanation and two distinct actions for both active
and inactive Sessions, verifies continued input after cancellation, and confirms
deletion of only its disposable Session. It identifies actions by their stable
IDs because UIKit exposes duplicated accessibility nodes for these buttons.
Both screenshots were visually reviewed and the original two fixture Sessions
remain intact. All 79 XCTest checks pass without skips or runtime warnings
(`Test-Voytty-2026.10.23_11-53-48-+0800.xcresult`), including both title translations
and real-host acceptance. The unsigned device build and `just ci-policy` pass.

## Native settings and Session sheet — 2026-10-23

Settings now uses a grouped system Form and Sections. Language, theme, text size,
notifications, diagnostics, background explanation and About retain their order
and existing behavior. Host removal and occupied-Session takeover use centered
system alerts with named targets, explicit Cancel and destructive confirmation,
matching Session deletion.

The Session chip presents a native medium/large sheet with a grouped List,
pull-to-refresh, Done and a navigation-bar New Session action. The sheet can be
expanded or dismissed by dragging. Create/rename forms present from the sheet;
presentation ownership persists through child-form dismissal before terminal
keyboard focus returns. Connection notices wait until the sheet has closed.
The terminal renderer, selection controls, shortcut toolbar and Home cards
retain their existing behavior.

The initial full UI run passed eight scenarios and exposed three failures:
the settings test tapped the Form's labeled Switch wrapper instead of its
nested native control, and recovery/deletion cleanup mistook the sheet's
duplicated dismissal regions for a menu. The test now targets the native Switch
and leaves sheet cleanup to its own controls. Subsequent service acceptance
exposed the export sheet's dependence on lazy Section rows; its presentation
and countdown now live in a stable background of the settings Form. Export is
also a full-width button row. The notification/detail/export flow passes after
these changes (`Test-VoyttyUI-2026.10.23_13-36-51-+0800.xcresult`), including save,
immediate second export, cancellation and restoration of detailed logging to Off.
Assertions and bounded waits are retained; no automatic retries were added.
The takeover test also waits for input readiness after the confirmation
disappears, before sending its remote-shell assertion; dismissal alone does not
mean attachment has finished.

Recovery and deletion pass their targeted recipes after those fixes
(`Test-VoyttyUI-2026.10.23_13-40-54-+0800.xcresult` and
`Test-VoyttyUI-2026.10.23_13-42-21-+0800.xcresult`). Together with the eight passing
scenarios in `Test-VoyttyUI-2026.10.23_13-19-02-+0800.xcresult`, this covers all
11 UI scenarios. The medium/large sheet, grouped settings, host removal and
named takeover screenshots were visually reviewed. The Mac was locked during
final verification, so this is screenshot/XCUITest evidence, not a new manual
foreground gesture pass. Both original fixture Sessions remain intact.

All 79 XCTest checks, including fresh-ticket real-host acceptance, pass with no
failures, skips or recorded runtime warnings
(`Test-Voytty-2026.10.23_13-43-45-+0800.xcresult`). The unsigned device build and
`just ci-policy` pass. These changes affect Swift UI, UI acceptance and its
runner/docs; no shared Rust or Android product code was changed in this pass.

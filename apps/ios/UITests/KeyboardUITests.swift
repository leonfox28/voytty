import XCTest
import Photos
import UIKit
import CryptoKit

private final class PhotoIdentity: @unchecked Sendable {
    private let lock = NSLock()
    private var identifier: String?
    var value: String? { lock.withLock { identifier } }
    func set(_ value: String?) { lock.withLock { identifier = value } }
}

/// Opt-in only: the recipe requires explicit IDs for an already-paired
/// disposable host. All commands run in a newly created Session.
@MainActor
final class KeyboardUITests: XCTestCase, @unchecked Sendable {
    private let app = XCUIApplication()
    private let sessionName = "ios-keyboard-ui-" + UUID().uuidString.prefix(8)
    private var restoreSession = ""
    private var hostID = ""
    private var photoIDs: [String] = []
    private var photoIDsFile: URL { FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent("zterm-ui-photo-ids.json") }
    private var created = false
    private var fixture: [String: String] = [:]
    private var terminal: XCUIElement { app.descendants(matching: .any)["terminalSelection"].firstMatch }
    private var navigationBack: XCUIElement { app.navigationBars.buttons.firstMatch }
    private var pickerBack: XCUIElement { app.otherElements["Browse View (Picker)"].buttons["BackButton"].firstMatch }
    private var panelClose: XCUIElement { app.buttons["closeSessionPanel"] }
    private var panelBar: XCUIElement {
        app.navigationBars.matching(NSPredicate(format: "identifier IN %@", ["会话列表", "Sessions"])).firstMatch
    }

    private func openSessionPanel() {
        if !panelClose.exists { app.buttons["sessionChip"].tap() }
        XCTAssertTrue(panelClose.waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["newSession"].exists)
    }

    private func closeSessionPanel() {
        XCTAssertTrue(panelClose.waitForExistence(timeout: 5))
        panelClose.tap()
        XCTAssertTrue(panelClose.waitForNonExistence(timeout: 5))
        waitFor { self.app.buttons["sessionChip"].isHittable }
    }

    private func toggleSetting(_ element: XCUIElement) {
        // Form exposes the whole labeled row as a Switch. Tap the actual
        // nested control, rather than the noninteractive label at its center.
        let control = element.switches.firstMatch
        XCTAssertTrue(control.isHittable)
        control.tap()
    }

    override func setUpWithError() throws {
        try MainActor.assumeIsolated { try setUpFixture() }
    }

    private func setUpFixture() throws {
        continueAfterFailure = false
        executionTimeAllowance = 120
        let url = try XCTUnwrap(Bundle(for: Self.self).url(forResource: "UIAcceptance", withExtension: "json"))
        fixture = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: String])
        let host = try XCTUnwrap(fixture["host"])
        hostID = host
        restoreSession = try XCTUnwrap(fixture["restoreSession"])
        if let data = try? Data(contentsOf: photoIDsFile) { photoIDs = try JSONDecoder().decode([String].self, from: data) }
        app.launch()
        let home = app.buttons["host-" + host]
        if !home.exists {
            if panelClose.exists { closeSessionPanel() }
            XCTAssertTrue(app.buttons["sessionChip"].waitForExistence(timeout: 15))
            XCTAssertTrue(navigationBack.waitForExistence(timeout: 5))
            navigationBack.tap()
        }
        XCTAssertTrue(home.waitForExistence(timeout: 15), "explicit disposable host must already be paired")
        home.tap()
        XCTAssertTrue(app.buttons["sessionChip"].waitForExistence(timeout: 15))
        // The terminal controls appear before attachment finishes. Attaching
        // dismisses the Session panel, so wait before trying to open it.
        waitFor { self.app.buttons["uploadFile"].isEnabled }
        openSessionPanel()
        waitFor { self.app.buttons["newSession"].isEnabled }
        app.buttons["newSession"].tap()
        let field = app.textFields["sessionName"]
        XCTAssertTrue(field.waitForExistence(timeout: 5), app.debugDescription)
        field.tap(); field.typeText(sessionName)
        app.textFields["sessionDirectory"].tap(); app.textFields["sessionDirectory"].typeText("/tmp")
        created = true
        print("ZTERM_UI_SESSION " + sessionName)
        app.buttons["sessionEditorSubmit"].tap()
        XCTAssertTrue(field.waitForNonExistence(timeout: 15))
        XCTAssertTrue(panelClose.waitForNonExistence(timeout: 15))
        XCTAssertTrue(terminal.waitForExistence(timeout: 15))
        waitFor { (self.terminal.value as? String)?.contains(" % ") == true || (self.terminal.value as? String)?.contains("$ ") == true }
    }

    override func tearDownWithError() throws {
        try MainActor.assumeIsolated { try tearDownFixture() }
    }

    private func tearDownFixture() throws {
        if !photoIDs.isEmpty {
            let ids = photoIDs
            let deletion = expectation(description: "Remove disposable Photos assets")
            PHPhotoLibrary.shared().performChanges({
                PHAssetChangeRequest.deleteAssets(PHAsset.fetchAssets(withLocalIdentifiers: ids, options: nil))
            }, completionHandler: { success, error in
                XCTAssertTrue(success, error?.localizedDescription ?? "photo cleanup failed")
                deletion.fulfill()
            })
            let button = XCUIApplication(bundleIdentifier: "com.apple.springboard").buttons.matching(NSPredicate(format: "label BEGINSWITH 'Delete' OR label BEGINSWITH '删除'")).firstMatch
            if button.waitForExistence(timeout: 5) { button.tap() }
            wait(for: [deletion], timeout: 10)
            try? FileManager.default.removeItem(at: photoIDsFile)
        }
        guard created else { return }
        continueAfterFailure = true
        let connectionDismiss = app.alerts.buttons["dismissConnectionNotice"].firstMatch
        if connectionDismiss.exists { connectionDismiss.tap() }
        let deleteCancel = app.alerts.buttons["cancelDeleteSession"].firstMatch
        if deleteCancel.exists { deleteCancel.tap() }
        for id in ["cancelForgetHost", "cancelTakeoverSession"] {
            let cancel = app.alerts.buttons[id].firstMatch
            if cancel.exists { cancel.tap() }
        }
        let editorCancel = app.buttons["cancelSessionEditor"]
        if editorCancel.exists { editorCancel.tap() }
        let uploadError = app.alerts.buttons["closeUploadError"].firstMatch
        if uploadError.exists { uploadError.tap() }
        let uploadCancel = app.buttons["cancelUpload"].firstMatch
        if uploadCancel.exists { uploadCancel.tap() }
        // A system sheet also exposes dismissal regions. Its own Done/list
        // actions below own cleanup; only dismiss an independent menu here.
        if !panelClose.exists {
            let popover = app.otherElements["PopoverDismissRegion"].firstMatch
            if popover.exists, popover.isHittable {
                popover.coordinate(withNormalizedOffset: CGVector(dx: 0.05, dy: 0.1)).tap()
            }
        }
        if app.buttons["Cancel"].exists { app.buttons["Cancel"].firstMatch.tap() }
        if app.otherElements["Browse View (Picker)"].exists { app.terminate(); app.launch() }
        if !panelClose.exists, !app.buttons["sessionChip"].exists, navigationBack.exists { navigationBack.tap() }
        if app.buttons["host-" + hostID].exists {
            app.buttons["host-" + hostID].tap()
            waitFor { self.app.buttons["uploadFile"].isEnabled || self.panelClose.exists }
        }
        openSessionPanel()
        let actions = app.buttons.matching(NSPredicate(format: "label CONTAINS %@ AND identifier BEGINSWITH 'sessionActions-'", String(sessionName))).firstMatch
        if actions.waitForExistence(timeout: 5) {
            actions.tap()
            app.buttons.matching(NSPredicate(format: "label IN %@", ["删除", "Delete"])).firstMatch.tap()
            let confirm = app.alerts.buttons["confirmDeleteSession"].firstMatch
            XCTAssertTrue(confirm.waitForExistence(timeout: 5))
            confirm.tap()
            waitFor { !actions.exists }
        }
        let restore = app.buttons["session-" + restoreSession]
        if !restore.exists, app.buttons["sessionChip"].exists { openSessionPanel() }
        XCTAssertTrue(restore.waitForExistence(timeout: 10))
        restore.tap()
        waitFor { !self.app.buttons["newSession"].exists }
    }

    func testSessionDeletionUsesNativeAlert() throws {
        let actions = app.buttons.matching(NSPredicate(format: "label CONTAINS %@ AND identifier BEGINSWITH 'sessionActions-'", String(sessionName))).firstMatch
        let alert = app.alerts.firstMatch
        let confirm = alert.buttons["confirmDeleteSession"].firstMatch
        let cancel = alert.buttons["cancelDeleteSession"].firstMatch
        func presentConfirmation() {
            openSessionPanel()
            XCTAssertTrue(actions.waitForExistence(timeout: 5))
            actions.tap()
            app.buttons.matching(NSPredicate(format: "label IN %@", ["删除", "Delete"])).firstMatch.tap()
            XCTAssertTrue(alert.waitForExistence(timeout: 5), "deletion requires a native system Alert")
            XCTAssertTrue(confirm.waitForExistence(timeout: 5))
            XCTAssertTrue(cancel.isHittable)
            // Count distinct actions; UIKit can expose multiple accessibility nodes per button.
            XCTAssertEqual(Set(alert.buttons.allElementsBoundByIndex.map(\.identifier)),
                           ["cancelDeleteSession", "confirmDeleteSession"])
            XCTAssertTrue(alert.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", String(sessionName))).firstMatch.exists,
                          "confirmation must identify the selected Session")
            XCTAssertTrue(alert.staticTexts.matching(NSPredicate(format: "label IN %@", [
                "这会结束该会话及其中运行的进程。", "This ends the session and all processes running in it."
            ])).firstMatch.exists)
            XCTAssertEqual(alert.frame.midX, app.frame.midX, accuracy: 20)
            XCTAssertEqual(alert.frame.midY, app.frame.midY, accuracy: 80)
        }

        presentConfirmation()
        attachScreenshot("System deletion alert for the active Session")
        cancel.tap()
        XCTAssertTrue(alert.waitForNonExistence(timeout: 5))
        XCTAssertTrue(actions.exists, "dismissing the confirmation must preserve the Session")
        closeSessionPanel()
        app.typeText("printf 'UI_DELETE_%s\\n' CANCELLED\n")
        waitFor { (self.terminal.value as? String)?.contains("UI_DELETE_CANCELLED") == true }

        openSessionPanel()
        app.buttons["session-" + restoreSession].tap()
        waitFor { !self.app.buttons["newSession"].exists }
        presentConfirmation()
        attachScreenshot("System deletion alert for the inactive Session")
        confirm.tap()
        waitFor { !actions.exists }
        XCTAssertTrue(app.buttons["session-" + restoreSession].exists, "delete must leave the other Session intact")
    }

    func testNativeSessionSheetAndEditorTransitions() throws {
        app.typeText("panel_identity=$$; printf 'UI_PANEL_%s\\n' READY\n")
        waitFor { (self.terminal.value as? String)?.contains("UI_PANEL_READY") == true }
        app.buttons["keyboardToggle"].tap()
        XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: 5))
        openSessionPanel()
        XCTAssertTrue(app.keyboards.firstMatch.waitForNonExistence(timeout: 5))
        XCTAssertGreaterThan(panelBar.frame.minY, app.frame.height * 0.25, "the system sheet starts at its medium detent")
        attachScreenshot("Native Session sheet at medium height")
        panelBar.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.2)).press(forDuration: 0.05,
            thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.95)),
            withVelocity: .slow, thenHoldForDuration: 0)
        XCTAssertTrue(panelClose.waitForNonExistence(timeout: 5), "system swipe dismisses the Session sheet")
        XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: 5), "closing the sheet restores the terminal keyboard")
        app.typeText("test \"$panel_identity\" = \"$$\" && printf 'UI_PANEL_%s\\n' DISMISSED\n")
        waitFor { (self.terminal.value as? String)?.contains("UI_PANEL_DISMISSED") == true }
        app.buttons["keyboardToggle"].tap()
        XCTAssertTrue(app.keyboards.firstMatch.waitForNonExistence(timeout: 5))

        openSessionPanel()
        let mediumTop = panelBar.frame.minY
        panelBar.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.2)).press(forDuration: 0.05,
            thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.15)),
            withVelocity: .slow, thenHoldForDuration: 0)
        waitFor { self.panelBar.frame.minY < mediumTop - 100 }
        attachScreenshot("Native Session sheet expanded")
        app.buttons["newSession"].tap()
        XCTAssertTrue(app.textFields["sessionName"].waitForExistence(timeout: 5))
        app.buttons["cancelSessionEditor"].tap()
        XCTAssertTrue(app.textFields["sessionName"].waitForNonExistence(timeout: 5))
        XCTAssertTrue(panelClose.isHittable, "cancelling a child form returns to the Session list")

        let actions = app.buttons.matching(NSPredicate(format: "label CONTAINS %@ AND identifier BEGINSWITH 'sessionActions-'", String(sessionName))).firstMatch
        let actionsID = actions.identifier
        let sessionID = String(actionsID.dropFirst("sessionActions-".count))
        let renamed = "ios-keyboard-ui-" + UUID().uuidString.prefix(8)
        print("ZTERM_UI_SESSION " + renamed)
        for name in [renamed, sessionName] {
            app.buttons[actionsID].tap()
            app.buttons.matching(NSPredicate(format: "label IN %@", ["重命名", "Rename"])).firstMatch.tap()
            let field = app.textFields["sessionName"]
            XCTAssertTrue(field.waitForExistence(timeout: 5))
            field.tap(); field.typeKey("a", modifierFlags: .command); field.typeText(name)
            app.buttons["sessionEditorSubmit"].tap()
            XCTAssertTrue(field.waitForNonExistence(timeout: 10))
            waitFor { self.app.buttons[actionsID].label.contains(name) }
            XCTAssertTrue(panelClose.isHittable, "saving a rename keeps the Session sheet open")
        }
        app.buttons["session-" + restoreSession].tap()
        XCTAssertTrue(panelClose.waitForNonExistence(timeout: 10))
        openSessionPanel()
        app.buttons["session-" + sessionID].tap()
        XCTAssertTrue(panelClose.waitForNonExistence(timeout: 10))
        waitFor { self.app.buttons["uploadFile"].isEnabled }
        app.typeText("test \"$panel_identity\" = \"$$\" && printf 'UI_PANEL_%s\\n' SAME_SHELL\n")
        waitFor { (self.terminal.value as? String)?.contains("UI_PANEL_SAME_SHELL") == true }
    }

    func testNativeSettingsAndHostRemovalCancellation() throws {
        navigationBack.tap()
        let host = app.buttons["host-" + hostID]
        XCTAssertTrue(host.waitForExistence(timeout: 5))
        host.press(forDuration: 1)
        app.buttons.matching(NSPredicate(format: "label IN %@", ["移除主机", "Remove host"])).firstMatch.tap()
        let alert = app.alerts.firstMatch
        XCTAssertTrue(alert.waitForExistence(timeout: 5))
        XCTAssertTrue(alert.buttons["confirmForgetHost"].firstMatch.isHittable)
        XCTAssertTrue(alert.staticTexts.matching(NSPredicate(format: "label CONTAINS 'presentation-fixture'")).firstMatch.exists)
        attachScreenshot("Native host removal alert")
        alert.buttons["cancelForgetHost"].firstMatch.tap()
        XCTAssertTrue(alert.waitForNonExistence(timeout: 5))
        XCTAssertTrue(host.isHittable, "cancellation preserves the saved pairing")
        app.buttons["settings"].tap()
        XCTAssertTrue(app.segmentedControls["languageChoice"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.collectionViews["settingsForm"].exists || app.tables["settingsForm"].exists,
                      "settings use the system Form's list container")
        XCTAssertTrue(app.segmentedControls["themeChoice"].isHittable)
        XCTAssertTrue(app.sliders["fontSize"].exists)
        attachScreenshot("Native grouped settings Form")
        app.swipeUp()
        XCTAssertTrue(app.switches["diagnosticsDetail"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["diagnosticsExport"].exists)
        attachScreenshot("Native notification and diagnostic settings rows")
        navigationBack.tap()
        host.tap()
        waitFor { self.app.buttons["uploadFile"].isEnabled }
    }

    func testNativeNavigationBackAndEdgeSwipe() throws {
        let host = app.buttons["host-" + hostID]
        let bar = app.navigationBars.firstMatch
        XCTAssertTrue(navigationBack.isHittable, "Back is available without opening the Session panel")
        XCTAssertGreaterThanOrEqual(terminal.frame.minY, bar.frame.maxY - 1, "navigation must not cover terminal rows")
        app.typeText("nav_identity=$$; printf '\\033[2J\\033[HUI_NAV_%s\\n' READY\n")
        waitFor { (self.terminal.value as? String)?.contains("UI_NAV_READY") == true }
        attachScreenshot("Native navigation above the terminal")
        if !app.keyboards.firstMatch.exists { app.buttons["keyboardToggle"].tap() }
        XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: 5))
        XCTAssertTrue(navigationBack.isHittable)
        XCTAssertGreaterThanOrEqual(terminal.frame.minY, bar.frame.maxY - 1)
        XCTAssertLessThanOrEqual(terminal.frame.maxY, app.buttons["keyboardToggle"].frame.minY)
        attachScreenshot("Native navigation with the software keyboard")
        app.buttons["keyboardToggle"].tap()
        XCTAssertTrue(app.keyboards.firstMatch.waitForNonExistence(timeout: 5))

        // A slow partial swipe must cancel without detaching the live Session.
        edgeSwipe(to: 0.2, hold: 0.8)
        XCTAssertTrue(navigationBack.isHittable)
        XCTAssertFalse(host.isHittable)
        app.typeText("test \"$nav_identity\" = \"$$\" && printf 'UI_NAV_%s\\n' CANCELLED\n")
        waitFor { (self.terminal.value as? String)?.contains("UI_NAV_CANCELLED") == true }

        // The shell continues work while the app is at its root screen.
        app.typeText("printf 'UI_NAV_%s\\n' WAIT; sleep 2; printf 'UI_NAV_%s\\n' AWAY\n")
        waitFor { (self.terminal.value as? String)?.contains("UI_NAV_WAIT") == true }
        navigationBack.tap()
        XCTAssertTrue(host.waitForExistence(timeout: 5))
        app.buttons["settings"].tap()
        XCTAssertTrue(app.segmentedControls["languageChoice"].waitForExistence(timeout: 5))
        attachScreenshot("Settings with native Back")
        edgeSwipe()
        XCTAssertTrue(host.waitForExistence(timeout: 5))
        app.buttons["addHost"].tap()
        XCTAssertTrue(navigationBack.waitForExistence(timeout: 5))
        let allow = XCUIApplication(bundleIdentifier: "com.apple.springboard").buttons.matching(
            NSPredicate(format: "label IN %@", ["Allow", "允许", "OK", "好"])).firstMatch
        if allow.waitForExistence(timeout: 2) { allow.tap() }
        attachScreenshot("Scanner with native Back")
        navigationBack.tap()
        XCTAssertTrue(host.waitForExistence(timeout: 5))
        host.tap()
        waitFor { self.app.buttons["uploadFile"].isEnabled }
        waitFor { (self.terminal.value as? String)?.contains("UI_NAV_AWAY") == true }
        app.typeText("test \"$nav_identity\" = \"$$\" && printf 'UI_NAV_%s\\n' SAME_SHELL\n")
        waitFor { (self.terminal.value as? String)?.contains("UI_NAV_SAME_SHELL") == true }

        edgeSwipe()
        XCTAssertTrue(host.waitForExistence(timeout: 5), "terminal edge swipe must win over its scroll gestures")
        app.terminate(); app.launch()
        XCTAssertTrue(host.waitForExistence(timeout: 15), "Back clears automatic terminal restoration")
        XCTAssertFalse(app.buttons["sessionChip"].exists)
        host.tap()
        waitFor { self.app.buttons["uploadFile"].isEnabled }
        XCTAssertTrue((terminal.value as? String)?.contains("UI_NAV_SAME_SHELL") == true)
    }

    private func edgeSwipe(to fraction: CGFloat = 0.85, hold: TimeInterval = 0) {
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.002, dy: 0.45))
            .press(forDuration: 0.05,
                   thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: fraction, dy: 0.45)),
                   withVelocity: .slow, thenHoldForDuration: hold)
    }

    func testNativeFileUploadAndLimit() throws {
        let filename = try XCTUnwrap(fixture["uploadFile"]), digest = try XCTUnwrap(fixture["uploadSHA256"])
        let oversized = try XCTUnwrap(fixture["oversizedFile"])
        app.typeText("stty -echo; printf '\\033[2J\\033[HUI_UPLOAD_%s\\n' READY; IFS= read -r uploaded; actual=$(shasum -a 256 \"$uploaded\"); case \"$actual\" in '\(digest) '*) printf 'UI_UPLOAD_%s\\n' EXACT;; *) printf 'UI_UPLOAD_%s\\n' BAD;; esac; rm -f -- \"$uploaded\"; rmdir -- \"${uploaded%/*}\"; stty echo\n")
        waitFor { (self.terminal.value as? String)?.contains("UI_UPLOAD_READY") == true }
        pickFile(oversized)
        let alert = app.alerts.firstMatch
        let failure = alert.staticTexts.matching(NSPredicate(format: "label CONTAINS '50 MB'")).firstMatch
        XCTAssertTrue(failure.waitForExistence(timeout: 10), app.debugDescription)
        attachScreenshot("System alert for oversized-file rejection")
        alert.buttons["retryUpload"].firstMatch.tap()
        XCTAssertTrue(failure.waitForExistence(timeout: 10), "retry must return to the native error alert")
        waitFor { alert.buttons["closeUploadError"].firstMatch.isHittable }
        alert.buttons["closeUploadError"].firstMatch.tap()
        XCTAssertTrue(alert.waitForNonExistence(timeout: 5))
        pickFile(filename)
        waitFor { self.app.buttons["uploadFile"].isEnabled && !self.app.otherElements["uploadProgress"].exists }
        XCTAssertFalse((terminal.value as? String)?.contains("UI_UPLOAD_EXACT") == true, "picker upload must not press Enter")
        app.typeText("\n")
        waitFor { (self.terminal.value as? String)?.contains("UI_UPLOAD_EXACT") == true || (self.terminal.value as? String)?.contains("UI_UPLOAD_BAD") == true }
        XCTAssertTrue((terminal.value as? String)?.contains("UI_UPLOAD_EXACT") == true, terminal.value as? String ?? "no output")
        attachScreenshot("Native document upload SHA-256 verified on host")
        app.buttons["uploadFile"].tap()
        let cancel = app.buttons.matching(NSPredicate(format: "label IN %@", ["Cancel", "取消"])).firstMatch
        XCTAssertTrue(cancel.waitForExistence(timeout: 5)); cancel.tap()
        waitFor { self.app.buttons["uploadFile"].isEnabled }
    }

    func testNativePhotoUpload() throws {
        XCTAssertEqual(PHPhotoLibrary.authorizationStatus(for: .readWrite), .authorized,
                       "the recipe grants the test runner access only to create/remove its fixture photo")
        let format = UIGraphicsImageRendererFormat(); format.scale = 1
        let data = try XCTUnwrap(UIGraphicsImageRenderer(size: CGSize(width: 160, height: 100), format: format).image { context in
            UIColor.green.setFill(); context.fill(CGRect(x: 0, y: 0, width: 160, height: 100))
            UIColor.purple.setFill(); context.fill(CGRect(x: 50, y: 25, width: 60, height: 50))
        }.pngData())
        let identity = PhotoIdentity()
        let creation = expectation(description: "Create disposable picker image")
        PHPhotoLibrary.shared().performChanges({
            let request = PHAssetCreationRequest.forAsset()
            request.addResource(with: .photo, data: data, options: nil)
            identity.set(request.placeholderForCreatedAsset?.localIdentifier)
        }, completionHandler: { success, error in
            XCTAssertTrue(success, error?.localizedDescription ?? "photo creation failed")
            creation.fulfill()
        })
        wait(for: [creation], timeout: 10)
        let id = try XCTUnwrap(identity.value)
        photoIDs.append(id)
        try JSONEncoder().encode(photoIDs).write(to: photoIDsFile, options: .atomic)
        let digest = SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
        app.typeText("stty -echo; printf '\\033[2J\\033[HUI_PHOTO_%s\\n' READY; IFS= read -r uploaded; actual=$(shasum -a 256 \"$uploaded\"); case \"$actual\" in '\(digest) '*) printf 'UI_PHOTO_%s\\n' EXACT;; *) printf 'UI_PHOTO_%s\\n' BAD;; esac; rm -f -- \"$uploaded\"; rmdir -- \"${uploaded%/*}\"; stty echo\n")
        waitFor { (self.terminal.value as? String)?.contains("UI_PHOTO_READY") == true }
        app.buttons["uploadImage"].tap()
        let photo = app.images.matching(NSPredicate(format: "label BEGINSWITH 'Photo,' OR label BEGINSWITH '照片,'")).firstMatch
        XCTAssertTrue(photo.waitForExistence(timeout: 10), app.debugDescription)
        attachScreenshot("System photo picker with disposable fixture")
        photo.tap()
        waitFor { self.app.buttons["uploadImage"].isEnabled }
        XCTAssertFalse((terminal.value as? String)?.contains("UI_PHOTO_EXACT") == true)
        app.typeText("\n")
        waitFor { (self.terminal.value as? String)?.contains("UI_PHOTO_EXACT") == true || (self.terminal.value as? String)?.contains("UI_PHOTO_BAD") == true }
        XCTAssertTrue((terminal.value as? String)?.contains("UI_PHOTO_EXACT") == true, terminal.value as? String ?? "no output")
        attachScreenshot("Native photo upload verified on host")
    }

    private func pickFile(_ filename: String) {
        app.buttons["uploadFile"].tap()
        // Files hides known extensions in labels; match the exact stem plus
        // delimiter so the oversized sibling cannot satisfy this selection.
        let stem = (filename as NSString).deletingPathExtension
        let file = app.cells.matching(NSPredicate(format: "label BEGINSWITH %@ OR label BEGINSWITH %@", filename + ",", stem + ",")).firstMatch
        // The picker remembers its folder. Wait for that content before trying
        // to navigate; the app's own Back remains in AX behind the modal.
        if !file.waitForExistence(timeout: 3) {
            let browse = app.buttons.matching(NSPredicate(format: "label IN %@", ["Browse", "浏览"])).firstMatch
            if browse.waitForExistence(timeout: 3) { browse.tap() }
            let local = app.descendants(matching: .any).matching(NSPredicate(format: "label IN %@", ["On My iPhone", "我的 iPhone", "我的iPhone"])).firstMatch
            if !local.waitForExistence(timeout: 3) {
                if pickerBack.exists { pickerBack.tap() }
            }
            if local.waitForExistence(timeout: 3) { local.tap() }
        }
        if !file.exists { app.swipeUp() }
        XCTAssertTrue(file.waitForExistence(timeout: 5), app.debugDescription)
        file.tap()
        XCTAssertTrue(file.waitForNonExistence(timeout: 10))
    }

    func testOccupiedTakeoverAndEndedRecovery() throws {
        print("ZTERM_UI_TAKEOVER " + sessionName)
        let alert = app.alerts.firstMatch
        let confirm = alert.buttons["confirmTakeoverSession"].firstMatch
        XCTAssertTrue(confirm.waitForExistence(timeout: 15), app.debugDescription)
        XCTAssertTrue(["已在其他设备连接", "Connected on another device"].contains(alert.label), alert.label)
        attachScreenshot("System alert after actual control loss")
        alert.buttons["dismissConnectionNotice"].firstMatch.tap()
        XCTAssertTrue(alert.waitForNonExistence(timeout: 5))
        let retry = app.buttons["connectionRetry"]
        XCTAssertFalse(retry.exists, "dismissing control loss leaves terminal output unobscured")
        XCTAssertTrue(terminal.exists)
        XCTAssertFalse(app.buttons["uploadFile"].isEnabled, "cancel must not take control")
        openSessionPanel()
        XCTAssertFalse(alert.exists, "refreshing the session list must not repeat the alert")
        let occupied = app.buttons.matching(NSPredicate(format: "label CONTAINS %@ AND identifier BEGINSWITH 'session-'", String(sessionName))).firstMatch
        occupied.tap()
        XCTAssertTrue(alert.buttons["cancelTakeoverSession"].firstMatch.waitForExistence(timeout: 5))
        XCTAssertTrue(alert.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", String(sessionName))).firstMatch.exists)
        attachScreenshot("Named system takeover alert inside the Session sheet")
        alert.buttons["cancelTakeoverSession"].firstMatch.tap()
        XCTAssertTrue(alert.waitForNonExistence(timeout: 5))
        XCTAssertTrue(panelClose.isHittable)
        occupied.tap()
        XCTAssertTrue(confirm.waitForExistence(timeout: 5)); confirm.tap()
        XCTAssertTrue(panelClose.waitForNonExistence(timeout: 10))
        waitFor { self.app.buttons["uploadFile"].isEnabled }
        app.typeText("printf 'UI_PANEL_TAKEOVER_%s\\n' OK\n")
        waitFor { (self.terminal.value as? String)?.contains("UI_PANEL_TAKEOVER_OK") == true }
        print("ZTERM_UI_TAKEOVER " + sessionName)
        XCTAssertTrue(alert.buttons["dismissConnectionNotice"].firstMatch.waitForExistence(timeout: 15))
        alert.buttons["dismissConnectionNotice"].firstMatch.tap()
        XCTAssertTrue(alert.waitForNonExistence(timeout: 5))
        navigationBack.tap()
        let host = app.buttons["host-" + hostID]
        XCTAssertTrue(host.waitForExistence(timeout: 5)); host.tap()
        XCTAssertTrue(confirm.waitForExistence(timeout: 15), app.debugDescription)
        XCTAssertTrue(["会话已被占用", "Session in use"].contains(alert.label), alert.label)
        attachScreenshot("System alert for occupied connection attempt")
        alert.buttons["dismissConnectionNotice"].firstMatch.tap()
        XCTAssertTrue(alert.waitForNonExistence(timeout: 5))
        XCTAssertFalse(app.buttons["uploadFile"].isEnabled)
        XCTAssertTrue(retry.exists)
        XCTAssertTrue(["接管", "Take over"].contains(retry.label), retry.label)
        retry.tap()
        XCTAssertTrue(confirm.waitForExistence(timeout: 5))
        attachScreenshot("Explicit takeover in system alert")
        confirm.tap()
        XCTAssertTrue(alert.waitForNonExistence(timeout: 15))
        XCTAssertTrue(retry.waitForNonExistence(timeout: 15))
        waitFor { self.app.buttons["uploadFile"].isEnabled }
        app.typeText("printf '\\nUI_TAKEOVER_%s\\n' OK\n")
        waitFor { (self.terminal.value as? String)?.contains("UI_TAKEOVER_OK") == true }
        app.typeText("printf '\\nUI_BACKGROUND_%s\\n' WAIT; sleep 2; printf '\\nUI_BACKGROUND_%s\\n' READY\n")
        waitFor { (self.terminal.value as? String)?.contains("UI_BACKGROUND_WAIT") == true }
        XCUIDevice.shared.press(.home)
        waitFor { self.app.state == .runningBackground || self.app.state == .runningBackgroundSuspended }
        // Keep the real app switch open while the remote shell emits output.
        // This checks survival of the process, not a promised iOS time budget.
        let terminated = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in self.app.state == .notRunning }, object: nil)
        terminated.isInverted = true
        XCTAssertEqual(XCTWaiter.wait(for: [terminated], timeout: 3), .completed)
        app.activate()
        waitFor { self.app.buttons["uploadFile"].isEnabled }
        waitFor { (self.terminal.value as? String)?.contains("UI_BACKGROUND_READY") == true }
        app.typeText("printf '\\nUI_RESUME_%s\\n' OK\n")
        waitFor { (self.terminal.value as? String)?.contains("UI_RESUME_OK") == true }
        attachScreenshot("Terminal after short background visit")
        app.typeText("exit\n")
        XCTAssertTrue(alert.waitForExistence(timeout: 10))
        let ended = alert.staticTexts.matching(NSPredicate(format: "label IN %@", ["会话已结束", "Session ended"])).firstMatch
        XCTAssertTrue(ended.exists, app.debugDescription)
        attachScreenshot("System alert after remote shell exit")
        alert.buttons["connectionAlertSessions"].firstMatch.tap()
        XCTAssertTrue(app.buttons["newSession"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["connectionRetry"].exists, "an ended session must not create a replacement via Retry")
    }

    func testConnectionFailureAlertAndRetry() throws {
        app.typeText("printf '\\nUI_FAILURE_%s\\n' BEFORE\n")
        waitFor { (self.terminal.value as? String)?.contains("UI_FAILURE_BEFORE") == true }
        navigationBack.tap()
        let host = app.buttons["host-" + hostID]
        XCTAssertTrue(host.waitForExistence(timeout: 5))
        print("ZTERM_UI_PAUSE " + sessionName)
        host.tap()
        let alert = app.alerts.firstMatch
        let retry = alert.buttons["retryConnectionAlert"].firstMatch
        XCTAssertTrue(retry.waitForExistence(timeout: 40), app.debugDescription)
        XCTAssertTrue(["连接失败", "Connection failed"].contains(alert.label), alert.label)
        attachScreenshot("System alert after real connection failure")
        print("ZTERM_UI_RESUME " + sessionName)
        retry.tap()
        XCTAssertTrue(alert.waitForNonExistence(timeout: 5))
        waitFor { self.app.buttons["uploadFile"].isEnabled }
        XCTAssertTrue((terminal.value as? String)?.contains("UI_FAILURE_BEFORE") == true)
        app.typeText("printf '\\nUI_FAILURE_%s\\n' AFTER\n")
        waitFor { (self.terminal.value as? String)?.contains("UI_FAILURE_AFTER") == true }
        XCTAssertFalse(alert.exists)
        attachScreenshot("Native Retry returns to the same remote session")
    }

    func testTransportReconnectPreservesSessionAndOutput() throws {
        app.typeText("printf '\\nUI_RECONNECT_%s\\n' BEFORE\n")
        waitFor { (self.terminal.value as? String)?.contains("UI_RECONNECT_BEFORE") == true }
        print("ZTERM_UI_PAUSE " + sessionName)
        let progress = app.descendants(matching: .any)["connectionProgress"].firstMatch
        XCTAssertTrue(progress.waitForExistence(timeout: 40), app.debugDescription)
        XCTAssertFalse(app.buttons["uploadFile"].isEnabled)
        XCTAssertFalse(app.alerts.firstMatch.exists, "automatic reconnection does not interrupt with an alert")
        attachScreenshot("Real transport timeout reconnecting")
        print("ZTERM_UI_RESUME " + sessionName)
        XCTAssertTrue(progress.waitForNonExistence(timeout: 15))
        waitFor { self.app.buttons["uploadFile"].isEnabled }
        app.typeText("printf '\\nUI_RECONNECT_%s\\n' AFTER\n")
        waitFor { (self.terminal.value as? String)?.contains("UI_RECONNECT_AFTER") == true }
        XCTAssertTrue((terminal.value as? String)?.contains("UI_RECONNECT_BEFORE") == true)
        XCTAssertFalse(app.alerts.firstMatch.exists)
        attachScreenshot("Reconnected to the same remote shell")
    }

    private func attachScreenshot(_ name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name; attachment.lifetime = .keepAlways; add(attachment)
    }

    func testNativeHardwareRoutingWithHiddenAndVisibleKeyboard() throws {
        let program = #"""
import os,termios,tty,select,time
old=termios.tcgetattr(0)
try:
    tty.setraw(0)
    os.write(1,b'\x1b[2J\x1b[H\x1b[?1lUI_KEYS_READY\r\n')
    data=b''; deadline=time.monotonic()+60; reported=False
    hardware=b'\x03\x1bb\x1b[Z\x1b[H\x1b[F\x1b[3~\x1b[5~\x1b[6~\x1bOP\x1b[24~\x1b[A\r\x7f'
    while not data.endswith(b'~END~') and time.monotonic()<deadline:
        if select.select([0],[],[],0.1)[0]: data+=os.read(0,4096)
        if not reported and len(data)>=len(hardware):
            os.write(1,b'UI_HARDWARE_DONE\r\n'); reported=True
    expected=hardware+b'visiblehidden~END~'
    os.write(1,b'UI_KEYS_OK\r\n' if data==expected else b'UI_KEYS_BAD:'+data.hex().encode()+b'\r\n')
    os.write(1,b'\x1b[>3uUI_ENHANCED_READY\r\n')
    data=b''; deadline=time.monotonic()+12
    while not data.endswith(b'~END~') and time.monotonic()<deadline:
        if select.select([0],[],[],0.1)[0]: data+=os.read(0,4096)
    expected=b'\x1b[99;5u\x1b[99;5:3u~END~'
    os.write(1,b'UI_ENHANCED_OK\r\n' if data==expected else b'UI_ENHANCED_BAD:'+data.hex().encode()+b'\r\n')
finally:
    os.write(1,b'\x1b[<u')
    termios.tcsetattr(0,termios.TCSANOW,old)
"""#
        let encoded = Data(program.utf8).base64EncodedString()
        // No keyboard-toggle tap: the terminal must already accept hardware input.
        app.typeText("python3 -c \"import base64;exec(base64.b64decode('\(encoded)'))\"\n")
        waitFor { (self.terminal.value as? String)?.contains("UI_KEYS_READY") == true }
        app.typeKey("c", modifierFlags: .control)
        app.typeKey("b", modifierFlags: .option)
        app.typeKey(.tab, modifierFlags: .shift)
        // The recipe injects exact HID usages through the simulator's keyboard
        // service: Xcode 27's typeKey mistranslates several AppKit special keys.
        print("ZTERM_UI_KEYBOARD_NAVIGATION")
        waitFor { (self.terminal.value as? String)?.contains("UI_HARDWARE_DONE") == true }
        app.buttons["keyboardToggle"].tap()
        app.typeText("visible")
        app.buttons["keyboardToggle"].tap()
        app.typeText("hidden~END~")
        waitFor { (self.terminal.value as? String)?.contains("UI_KEYS_OK") == true || (self.terminal.value as? String)?.contains("UI_KEYS_BAD") == true }
        XCTAssertTrue((terminal.value as? String)?.contains("UI_KEYS_OK") == true, terminal.value as? String ?? "no terminal text")
        waitFor { (self.terminal.value as? String)?.contains("UI_ENHANCED_READY") == true }
        app.typeKey("c", modifierFlags: .control)
        app.typeText("~END~")
        waitFor { (self.terminal.value as? String)?.contains("UI_ENHANCED_OK") == true || (self.terminal.value as? String)?.contains("UI_ENHANCED_BAD") == true }
        XCTAssertTrue((terminal.value as? String)?.contains("UI_ENHANCED_OK") == true, terminal.value as? String ?? "no terminal text")
        let image = XCTAttachment(screenshot: app.screenshot())
        image.name = "Native keyboard routing to disposable host"
        image.lifetime = .keepAlways
        add(image)
    }

    func testNotificationPermissionDeliveryAndNativeDiagnosticExport() throws {
        navigationBack.tap()
        app.buttons["settings"].tap()
        let notifications = app.switches["terminalNotifications"]
        XCTAssertTrue(notifications.waitForExistence(timeout: 5))
        if notifications.value as? String != "1" {
            toggleSetting(notifications)
            let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
            let allow = springboard.buttons.matching(NSPredicate(format: "label IN %@", ["Allow", "允许"])).firstMatch
            if allow.waitForExistence(timeout: 3) { allow.tap() }
        }
        waitFor { notifications.value as? String == "1" }
        let detail = app.switches["diagnosticsDetail"]
        app.swipeUp()
        XCTAssertTrue(detail.waitForExistence(timeout: 5))
        let originallyDetailed = detail.value as? String == "1"
        if !originallyDetailed { toggleSetting(detail) }
        waitFor { detail.value as? String == "1" }
        app.buttons["diagnosticsExport"].tap()
        // Files remembers the last browsing location. Always save in this
        // run's disposable folder; the recipe validates the exported contents.
        let browser = app.otherElements["Browse View (Picker)"]
        XCTAssertTrue(browser.waitForExistence(timeout: 10))
        let folderName = try XCTUnwrap(fixture["exportDirectory"])
        let folder = browser.cells.matching(NSPredicate(format: "label BEGINSWITH %@", folderName + ",")).firstMatch
        if !folder.waitForExistence(timeout: 2) {
            let local = browser.descendants(matching: .any).matching(
                NSPredicate(format: "label IN %@", ["On My iPhone", "我的 iPhone", "我的iPhone"])).firstMatch
            let deadline = Date().addingTimeInterval(10)
            while !local.exists, pickerBack.exists, Date() < deadline { pickerBack.tap() }
            if local.exists { local.tap() }
        }
        XCTAssertTrue(folder.waitForExistence(timeout: 5), app.debugDescription)
        folder.tap()
        let save = app.buttons["DOCPicker.actionButton"]
        XCTAssertTrue(save.waitForExistence(timeout: 10), app.debugDescription)
        XCTAssertTrue(save.isEnabled)
        let picker = XCTAttachment(screenshot: app.screenshot())
        picker.name = "Native diagnostic document exporter"; picker.lifetime = .keepAlways; add(picker)
        save.tap()
        XCTAssertTrue(browser.waitForNonExistence(timeout: 10), app.debugDescription)
        waitFor { self.app.buttons["diagnosticsExport"].isEnabled }
        app.buttons["diagnosticsExport"].tap()
        XCTAssertTrue(browser.waitForExistence(timeout: 10), app.debugDescription)
        // The system places Cancel on the root Browse page, above location navigation.
        let cancel = app.buttons.matching(NSPredicate(format: "label IN %@", ["Cancel", "取消", "Close", "关闭"])).firstMatch
        waitFor { self.pickerBack.exists || cancel.exists }
        let cancelDeadline = Date().addingTimeInterval(10)
        while !cancel.exists, pickerBack.exists, Date() < cancelDeadline { pickerBack.tap() }
        XCTAssertTrue(cancel.waitForExistence(timeout: 5), app.debugDescription)
        cancel.tap()
        XCTAssertTrue(browser.waitForNonExistence(timeout: 10))
        waitFor { self.app.buttons["diagnosticsExport"].isEnabled }
        if !originallyDetailed { toggleSetting(detail); waitFor { detail.value as? String == "0" } }
        attachScreenshot("Simplified diagnostics settings after save and cancel")
        navigationBack.tap()
        app.buttons["host-" + hostID].tap()
        XCTAssertTrue(terminal.waitForExistence(timeout: 15))
        let marker = "iOS notification " + UUID().uuidString.prefix(8)
        app.typeText("printf '\\033]777;notify;zterm acceptance;\(marker)\\a'\n")
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        let notification = springboard.staticTexts[marker].firstMatch
        XCTAssertTrue(notification.waitForExistence(timeout: 10), springboard.debugDescription)
        let banner = XCTAttachment(screenshot: springboard.screenshot())
        banner.name = "OSC notification delivered by iOS"; banner.lifetime = .keepAlways; add(banner)
        notification.tap()
        XCTAssertTrue(terminal.waitForExistence(timeout: 10))
    }

    private func waitFor(_ predicate: @escaping () -> Bool, file: StaticString = #filePath, line: UInt = #line) {
        let expectation = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in predicate() }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [expectation], timeout: 15), .completed, file: file, line: line)
    }
}

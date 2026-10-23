import XCTest
import UIKit

/// Production SwiftUI/UIKit layouts on the pinned 12.9-inch iPad simulator. Transport is replaced
/// by the existing chat and viewport fixtures; these tests make no claims about a real host.
final class PhonePadUITests: XCTestCase {
    override func setUpWithError() throws {
        try super.setUpWithError()
        try XCTSkipUnless(UIDevice.current.userInterfaceIdiom == .pad, "iPad acceptance runs on its own simulator")
        continueAfterFailure = false
        XCUIDevice.shared.orientation = .portrait
    }

    override func tearDownWithError() throws {
        XCUIDevice.shared.orientation = .portrait
        XCUIApplication().terminate()
        try super.tearDownWithError()
    }

    func testSidebarSelectsClaudeAndCodexAndKeepsEachDraftAcrossRotation() {
        let app = launch(["-ui-chat-fixture"])
        let sidebar = app.descendants(matching: .any)["chat-session-sidebar"]
        let claude = app.buttons["chat-session-fixture-1"]
        let codex = app.buttons["chat-session-fixture-codex"]
        let input = app.textViews["chat-input"]
        XCTAssertTrue(sidebar.waitForExistence(timeout: 15))
        XCTAssertTrue(claude.isSelected && claude.isHittable)
        XCTAssertTrue(app.staticTexts["把构建结果整理成报告"].exists)
        assertSidebarBesideConversation(app, sidebar: sidebar)
        shot(app, "p01-portrait-claude-sidebar")

        input.tap()
        input.typeText("Claude iPad draft")
        rotate(app, to: .landscapeLeft)
        waitFor("Claude's draft survives rotation") { input.value as? String == "Claude iPad draft" }
        assertSidebarBesideConversation(app, sidebar: sidebar)
        XCTAssertLessThanOrEqual(input.frame.width, 820, "the composer stays within the readable column")
        assertInsideWindow(input, app: app)
        shot(app, "p02-landscape-claude-keyboard-draft")

        codex.tap()
        XCTAssertTrue(app.staticTexts["Does monthrange handle leap years?"].waitForExistence(timeout: 10))
        XCTAssertTrue(codex.isSelected && !claude.isSelected, "the sidebar names the visible session")
        XCTAssertEqual(input.value as? String, "", "Claude's draft belongs to Claude")
        input.tap()
        input.typeText("Codex iPad draft")
        shot(app, "p03-landscape-codex-keyboard-draft")

        claude.tap()
        waitFor("Claude's own draft is restored") { claude.isSelected && input.value as? String == "Claude iPad draft" }
        XCTAssertTrue(app.staticTexts["把构建结果整理成报告"].exists)
        codex.tap()
        waitFor("Codex's own draft is restored") { codex.isSelected && input.value as? String == "Codex iPad draft" }
        rotate(app, to: .portrait)
        waitFor("the selected conversation and its draft survive portrait") {
            codex.isSelected && input.value as? String == "Codex iPad draft"
        }
        assertSidebarBesideConversation(app, sidebar: sidebar)
        assertInsideWindow(input, app: app)
        shot(app, "p04-portrait-codex-keyboard-draft")
    }

    func testPreviewRendersHTMLSourceMarkdownAndImageOnIPad() {
        let app = launch(["-ui-chat-fixture"])
        XCTAssertTrue(app.descendants(matching: .any)["chat-session-sidebar"].waitForExistence(timeout: 15))
        let page = app.buttons["chat-preview-report.html"]
        XCTAssertTrue(page.waitForExistence(timeout: 5) && page.isHittable)
        page.tap()
        let web = app.webViews.firstMatch
        assertHTML(web)
        shot(app, "p05-portrait-html-preview")

        rotate(app, to: .landscapeLeft)
        assertHTML(web)
        let done = app.buttons["chat-preview-done"]
        XCTAssertTrue(done.isHittable)
        assertInsideWindow(done, app: app)
        app.segmentedControls.buttons["源码"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["chat-preview-source"].waitForExistence(timeout: 5))
        shot(app, "p06-landscape-html-source")
        app.segmentedControls.buttons["预览"].tap()
        assertHTML(web)
        done.tap()

        XCTAssertTrue(app.buttons["chat-session-fixture-1"].isSelected, "closing a preview keeps the conversation")
        app.buttons["chat-preview-notes.md"].tap()
        XCTAssertTrue(app.staticTexts["构建说明"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["模拟器里不能拍照。"].exists)
        shot(app, "p07-landscape-markdown-preview")
        app.buttons["chat-preview-done"].tap()
        app.buttons["chat-preview-chart.png"].tap()
        let image = app.descendants(matching: .any)["chat-preview-image"]
        XCTAssertTrue(image.waitForExistence(timeout: 5))
        assertInsideWindow(image, app: app)
        shot(app, "p08-landscape-image-preview")
        app.buttons["chat-preview-done"].tap()

        // The fixture acknowledges the actual send and starts a running turn, rather than merely
        // checking that a field was tappable. Stop leaves the session idle for the next user action.
        let input = app.textViews["chat-input"]
        input.tap()
        input.typeText("iPad preview reviewed")
        app.buttons["chat-send"].tap()
        XCTAssertTrue(app.staticTexts["iPad preview reviewed"].waitForExistence(timeout: 5))
        let stop = app.buttons["chat-stop"]
        XCTAssertTrue(stop.waitForExistence(timeout: 5))
        stop.tap()
        XCTAssertTrue(app.buttons["chat-continue"].waitForExistence(timeout: 5))
        shot(app, "p09-landscape-message-delivered-and-stopped")
    }

    func testRemoteCompanionPanelAndFullscreenFollowAllFourOrientations() {
        let app = launch(["-ui-viewport-fixture", "-ui-viewport-chat"])
        let viewport = app.descendants(matching: .any)["remote-viewport"]
        let panel = app.descendants(matching: .any)["ipad-chat-panel"]
        let input = app.staticTexts["fixture-remote-input"]
        XCTAssertTrue(viewport.waitForExistence(timeout: 15) && panel.waitForExistence(timeout: 5))
        waitFor("the real scene is portrait") { self.metrics(viewport)["interface_orientation"] == Double(UIInterfaceOrientation.portrait.rawValue) }
        assertCompanionPanel(app, viewport: viewport, panel: panel)
        XCTAssertEqual(input.value as? String, "none")
        shot(app, "p10-portrait-remote-and-chat")

        rotate(app, to: .landscapeLeft)
        let landscapeValues = [Double(UIInterfaceOrientation.landscapeLeft.rawValue), Double(UIInterfaceOrientation.landscapeRight.rawValue)]
        waitFor("the real scene is landscape") { landscapeValues.contains(self.metrics(viewport)["interface_orientation"] ?? 0) }
        let firstLandscape = metrics(viewport)["interface_orientation"]
        assertCompanionPanel(app, viewport: viewport, panel: panel)
        shot(app, "p11-landscape-remote-and-chat")

        let fullscreen = app.buttons["session-fullscreen"]
        fullscreen.tap()
        waitFor("fullscreen hides the companion panel") { fullscreen.label == "退出全屏" && !panel.exists }
        assertFullscreenPicture(app, viewport: viewport)
        XCTAssertEqual(input.value as? String, "none", "local fullscreen does not click the remote desktop")
        shot(app, "p12-landscape-fullscreen")

        rotate(app, to: .landscapeRight)
        waitFor("the scene rotates to the other landscape direction") {
            let value = self.metrics(viewport)["interface_orientation"] ?? 0
            return landscapeValues.contains(value) && value != firstLandscape
        }
        assertFullscreenPicture(app, viewport: viewport)
        shot(app, "p13-opposite-landscape-fullscreen")
        rotate(app, to: .portraitUpsideDown)
        waitFor("the scene accepts upside-down portrait") {
            self.metrics(viewport)["interface_orientation"] == Double(UIInterfaceOrientation.portraitUpsideDown.rawValue)
        }
        assertFullscreenPicture(app, viewport: viewport)
        shot(app, "p14-upside-down-portrait-fullscreen")
        rotate(app, to: .portrait)
        waitFor("the scene returns to portrait") { self.metrics(viewport)["interface_orientation"] == Double(UIInterfaceOrientation.portrait.rawValue) }
        assertFullscreenPicture(app, viewport: viewport)
        shot(app, "p15-portrait-fullscreen")

        fullscreen.tap()
        waitFor("leaving fullscreen restores the same companion conversation") { panel.exists && fullscreen.label == "全屏" }
        assertCompanionPanel(app, viewport: viewport, panel: panel)
        XCTAssertTrue(app.staticTexts["把构建结果整理成报告"].exists)
        XCTAssertEqual(input.value as? String, "none", "rotation and local controls do not become remote input")
        assertRemoteCenterClick(app, viewport: viewport)
        shot(app, "p16-portrait-panel-restored-remote-input")
    }

    func testCompanionExpandsToWorkspaceAndReopensTheSelectedConversation() {
        let app = launch(["-ui-viewport-fixture", "-ui-viewport-chat"])
        let panel = app.descendants(matching: .any)["ipad-chat-panel"]
        XCTAssertTrue(panel.waitForExistence(timeout: 15))
        app.buttons["chat-expand"].tap()
        let workspace = app.descendants(matching: .any)["ipad-chat-workspace"]
        XCTAssertTrue(workspace.waitForExistence(timeout: 5))
        XCTAssertFalse(panel.exists)
        let sidebar = app.descendants(matching: .any)["chat-session-sidebar"]
        XCTAssertTrue(sidebar.waitForExistence(timeout: 5))
        let codex = app.buttons["chat-session-fixture-codex"]
        codex.tap()
        XCTAssertTrue(app.staticTexts["Does monthrange handle leap years?"].waitForExistence(timeout: 10))
        XCTAssertTrue(codex.isSelected)
        let input = app.textViews["chat-input"]
        input.tap()
        input.typeText("Expanded iPad draft")
        shot(app, "p17-expanded-workspace-codex-draft")

        app.buttons["chat-close"].tap()
        waitFor("closing the workspace returns to the remote desktop") { !workspace.exists && !panel.exists }
        let open = app.buttons["session-chat"]
        XCTAssertTrue(open.waitForExistence(timeout: 5) && open.isHittable)
        open.tap()
        XCTAssertTrue(panel.waitForExistence(timeout: 5))
        waitFor("the companion reopens the selected Codex session and draft") {
            panel.textViews["chat-input"].exists && panel.textViews["chat-input"].value as? String == "Expanded iPad draft"
                && app.staticTexts["Does monthrange handle leap years?"].exists
        }
        shot(app, "p18-reopened-companion-same-codex-draft")
    }

    /// The fixture resizes the production root to 683 points in portrait and the whole window in
    /// landscape. This covers container changes; it is not a manual system Split View acceptance.
    func testNarrowContainerMovesChatBetweenSheetAndInlineWithoutLosingItsDraft() {
        XCUIDevice.shared.orientation = .landscapeLeft
        let app = launch(["-ui-viewport-fixture", "-ui-viewport-chat", "-ui-viewport-resize"])
        let panel = app.descendants(matching: .any)["ipad-chat-panel"]
        let sheet = app.descendants(matching: .any)["chat-sheet"]
        XCTAssertTrue(panel.waitForExistence(timeout: 15))
        let input = app.textViews["chat-input"]
        XCTAssertTrue(input.waitForExistence(timeout: 5))
        input.tap()
        input.typeText("Container resize draft")
        rotate(app, to: .portrait)
        waitFor("a narrow container presents chat as a sheet") { !panel.exists && sheet.exists }
        waitFor("the sheet retains the current draft") { input.value as? String == "Container resize draft" }
        XCTAssertTrue(app.buttons["chat-close"].isHittable)
        shot(app, "p19-narrow-683-chat-sheet")

        rotate(app, to: .landscapeLeft)
        waitFor("widening the container moves the open sheet back inline") { !sheet.exists && panel.exists }
        waitFor("the migration keeps chat open with its draft") {
            panel.textViews["chat-input"].exists && panel.textViews["chat-input"].value as? String == "Container resize draft"
        }
        let viewport = app.descendants(matching: .any)["remote-viewport"]
        assertCompanionPanel(app, viewport: viewport, panel: panel)
        XCTAssertEqual(app.staticTexts["fixture-remote-input"].value as? String, "none")
        shot(app, "p20-wide-container-restores-open-chat")
    }

    private func launch(_ arguments: [String]) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = arguments + ["-AppleLanguages", "(zh-Hans)", "-AppleLocale", "zh_CN"]
        app.launch()
        return app
    }

    private func rotate(_ app: XCUIApplication, to orientation: UIDeviceOrientation) {
        XCUIDevice.shared.orientation = orientation
        waitFor("window geometry after rotation") {
            let frame = app.windows.firstMatch.frame
            return orientation.isLandscape ? frame.width > frame.height : frame.height > frame.width
        }
    }

    private func assertSidebarBesideConversation(_ app: XCUIApplication, sidebar: XCUIElement) {
        // UIKit can expose a ScrollView's frame before its split-column content inset.
        // The actual editable text view gives the visible conversation's global bounds.
        let column = app.textViews["chat-input"]
        XCTAssertTrue(column.waitForExistence(timeout: 5))
        XCTAssertGreaterThan(sidebar.frame.width, 200)
        XCTAssertLessThanOrEqual(sidebar.frame.maxX, column.frame.minX + 2, "sessions remain beside the conversation")
        XCTAssertGreaterThan(column.frame.width, 400)
        XCTAssertLessThanOrEqual(column.frame.width, 820)
        assertInsideWindow(column, app: app)
        assertInsideWindow(sidebar, app: app)
    }

    private func assertCompanionPanel(_ app: XCUIApplication, viewport: XCUIElement, panel: XCUIElement) {
        waitFor("remote desktop and companion panel are side by side") {
            viewport.frame.width > 0 && panel.frame.width >= 320 && viewport.frame.maxX <= panel.frame.minX + 2
        }
        XCTAssertFalse(app.descendants(matching: .any)["chat-sheet"].exists, "the full-size 12.9-inch layout keeps chat inline")
        XCTAssertTrue(panel.textViews["chat-input"].exists)
        assertInsideWindow(panel, app: app)
        assertFittedPicture(viewport)
    }

    private func assertFullscreenPicture(_ app: XCUIApplication, viewport: XCUIElement) {
        waitFor("the fullscreen viewport fills the window") {
            let value = self.metrics(viewport), window = app.windows.firstMatch.frame
            return (value["viewport_width"] ?? 0) >= window.width * 0.95
                && (value["viewport_height"] ?? 0) >= window.height * 0.95
        }
        XCTAssertTrue(app.buttons["session-fullscreen"].isHittable)
        XCTAssertTrue(app.buttons["session-agent"].isHittable)
        assertFittedPicture(viewport)
    }

    private func assertFittedPicture(_ viewport: XCUIElement) {
        waitFor("the complete picture fits its available area") {
            let value = self.metrics(viewport)
            guard let x = value["displayed_x"], let y = value["displayed_y"],
                  let width = value["displayed_width"], let height = value["displayed_height"],
                  let ax = value["available_x"], let ay = value["available_y"],
                  let aw = value["available_width"], let ah = value["available_height"] else { return false }
            return width > 0 && height > 0 && x >= ax - 1 && y >= ay - 1
                && x + width <= ax + aw + 1 && y + height <= ay + ah + 1
                && (abs(width - aw) <= 2 || abs(height - ah) <= 2)
        }
    }

    private func assertRemoteCenterClick(_ app: XCUIApplication, viewport: XCUIElement) {
        let value = metrics(viewport)
        let x = ((value["displayed_x"] ?? 0) + (value["displayed_width"] ?? 0) / 2) / (value["viewport_width"] ?? 1)
        let y = ((value["displayed_y"] ?? 0) + (value["displayed_height"] ?? 0) / 2) / (value["viewport_height"] ?? 1)
        viewport.coordinate(withNormalizedOffset: CGVector(dx: x, dy: y)).tap()
        let input = app.staticTexts["fixture-remote-input"]
        waitFor("one mapped remote click") { self.observation(input)["sequence"] as? Int == 1 }
        let args = observation(input)["args"] as? [String: Any] ?? [:]
        XCTAssertEqual(args["action"] as? String, "click")
        XCTAssertEqual((args["x"] as? NSNumber)?.doubleValue ?? -1, 1280, accuracy: 4)
        XCTAssertEqual((args["y"] as? NSNumber)?.doubleValue ?? -1, 540, accuracy: 4)
    }

    private func assertHTML(_ web: XCUIElement) {
        XCTAssertTrue(web.waitForExistence(timeout: 5))
        // The existing iPhone acceptance measured a 34.8 s cold WebContent launch on iOS 26.2.
        XCTAssertTrue(web.staticTexts["构建报告"].waitForExistence(timeout: 45), "wait for rendered HTML, not just a web view")
        XCTAssertTrue(web.staticTexts["脚本没有运行"].exists)
    }

    private func assertInsideWindow(_ element: XCUIElement, app: XCUIApplication) {
        let frame = element.frame, window = app.windows.firstMatch.frame
        XCTAssertGreaterThan(frame.width, 0)
        XCTAssertGreaterThanOrEqual(frame.minX, window.minX - 1)
        XCTAssertLessThanOrEqual(frame.maxX, window.maxX + 1)
        XCTAssertGreaterThanOrEqual(frame.minY, window.minY - 1)
        XCTAssertLessThanOrEqual(frame.maxY, window.maxY + 1)
    }

    private func observation(_ element: XCUIElement) -> [String: Any] {
        guard let text = element.value as? String, let data = text.data(using: .utf8),
              let value = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return [:] }
        return value
    }

    private func metrics(_ viewport: XCUIElement) -> [String: Double] {
        observation(viewport).compactMapValues { ($0 as? NSNumber)?.doubleValue }
    }

    private func waitFor(_ description: String, timeout: TimeInterval = 15, _ condition: @escaping () -> Bool) {
        let predicate = NSPredicate { _, _ in condition() }
        let expectation = XCTNSPredicateExpectation(predicate: predicate, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [expectation], timeout: timeout), .completed, description)
    }

    private func shot(_ app: XCUIApplication, _ name: String) {
        let image = XCUIScreen.main.screenshot()
        let attachment = XCTAttachment(screenshot: image)
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
        let directory = URL(fileURLWithPath: "/tmp/ios-pad")
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try? image.pngRepresentation.write(to: directory.appendingPathComponent(name + ".png"))
        let frame = app.windows.firstMatch.frame
        let picture = image.image
        let diagnostics: [String: Any] = [
            "window": ["x": frame.minX, "y": frame.minY, "width": frame.width, "height": frame.height],
            "screenshot": ["width": picture.size.width, "height": picture.size.height,
                           "orientation": picture.imageOrientation.rawValue],
            "device_orientation": XCUIDevice.shared.orientation.rawValue,
        ]
        if let data = try? JSONSerialization.data(withJSONObject: diagnostics, options: [.sortedKeys]) {
            try? data.write(to: directory.appendingPathComponent(name + ".json"))
        }
    }
}

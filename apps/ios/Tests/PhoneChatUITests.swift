import XCTest

/// The chat view over the `-ui-chat-fixture` demo events (no host): send, stop, the session list and
/// the new-chat form.
final class PhoneChatUITests: XCTestCase {
    func testChatSendStopListAndNewChatForm() {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments = ["-ui-chat-fixture"]
        app.launch()
        defer { app.terminate() }

        XCTAssertTrue(app.staticTexts["看一下 README 里构建步骤还对不对"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.staticTexts["chat-turn-end-1"].exists)
        XCTAssertFalse(app.buttons["chat-stop"].exists, "no stop button while idle")
        let send = app.buttons["chat-send"]
        XCTAssertFalse(send.isEnabled, "nothing to send yet")
        attach(app, "conversation")

        let input = app.textViews["chat-input"]
        input.tap()
        input.typeText("再跑一次测试")
        XCTAssertTrue(send.isEnabled)
        send.tap()
        XCTAssertTrue(app.staticTexts["再跑一次测试"].waitForExistence(timeout: 5), "the sent message shows as a user line")
        XCTAssertEqual(input.value as? String ?? "", "", "sending clears the field")
        let stop = app.buttons["chat-stop"]
        XCTAssertTrue(stop.waitForExistence(timeout: 5), "a running turn shows the stop button")
        attach(app, "running")
        stop.tap()
        XCTAssertTrue(app.staticTexts["chat-turn-end-2"].waitForExistence(timeout: 5))
        XCTAssertTrue((app.staticTexts["chat-turn-end-2"].label).hasPrefix("已停止"), app.staticTexts["chat-turn-end-2"].label)
        XCTAssertFalse(stop.exists)

        // Return sends when nothing is being composed.
        input.tap()
        input.typeText("回车发送\n")
        XCTAssertTrue(app.staticTexts["回车发送"].waitForExistence(timeout: 5))
        app.buttons["chat-stop"].tap()

        app.buttons["chat-back"].tap()
        XCTAssertTrue(app.buttons["chat-session-fixture-1"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["chat-session-fixture-2"].exists)
        attach(app, "sessions")

        app.buttons["chat-new"].tap()
        XCTAssertTrue(app.navigationBars["新对话"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["chat-new-start"].isEnabled, "a usable agent and the home folder are preselected")
        attach(app, "new chat")
        app.buttons["chat-new-start"].tap()
        XCTAssertTrue(app.alerts["没能启动"].waitForExistence(timeout: 5), "a refused start says why")
        app.alerts["没能启动"].buttons.firstMatch.tap()
    }

    private func attach(_ app: XCUIApplication, _ name: String) {
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = name; shot.lifetime = .keepAlways
        add(shot)
    }
}

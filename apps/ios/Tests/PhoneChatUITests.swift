import XCTest

/// The chat view over the `-ui-chat-fixture` demo events (no host): the rows (run, diff card, plan,
/// question, context ring), sending, steering, queueing, stopping and continuing, the session list and
/// the new-chat form.
final class PhoneChatUITests: XCTestCase {
    func testChatRowsSteerQueueStopListAndNewChatForm() {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments = ["-ui-chat-fixture"]
        app.launch()
        defer { app.terminate() }

        // Rows: the user's message, the run summarized in one sentence, the diff card, the plan, the question.
        XCTAssertTrue(app.staticTexts["看一下 README 里构建步骤还对不对"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.staticTexts["读了 2 个文件，运行了 1 条命令"].exists, "the run's sentence")
        XCTAssertTrue(app.staticTexts["1 个失败"].exists, "the run counts its failed call")
        XCTAssertTrue(app.staticTexts["完成 2/3"].exists, "the plan's progress")
        XCTAssertTrue(app.descendants(matching: .any)["chat-context"].exists, "the context ring")
        attach(app, "rows")

        let diff = app.buttons["chat-diff"]
        XCTAssertTrue(diff.exists)
        diff.tap()
        XCTAssertTrue(app.staticTexts["Run rustup target add aarch64-apple-ios aarch64-apple-ios-sim."].waitForExistence(timeout: 5), "the diff opens")
        diff.tap()

        app.buttons["chat-option-1"].tap()
        XCTAssertTrue(app.staticTexts["已回答：要"].waitForExistence(timeout: 5), "a numbered option answers the question")

        // Sending starts a turn; while it runs, sending adds to it (steer).
        let input = app.textViews["chat-input"]
        input.tap()
        input.typeText("再跑一次测试")
        app.buttons["chat-send"].tap()
        XCTAssertTrue(app.staticTexts["再跑一次测试"].waitForExistence(timeout: 5))
        let stop = app.buttons["chat-stop"]
        XCTAssertTrue(stop.waitForExistence(timeout: 5), "a running turn shows the stop button")
        XCTAssertEqual(app.buttons["chat-mode"].label, "插话", "steer is the default while the agent works")
        input.tap()
        input.typeText("用 release 配置")
        app.buttons["chat-send"].tap()
        XCTAssertTrue(app.staticTexts["用 release 配置"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.descendants(matching: .any)["chat-steer-tag"].exists, "the steer is flagged")
        attach(app, "steer")

        // The chip switches to queueing; a queued message can be taken back.
        app.buttons["chat-mode"].tap()
        XCTAssertEqual(app.buttons["chat-mode"].label, "排队")
        input.tap()
        input.typeText("跑完再看文档")
        app.buttons["chat-send"].tap()
        let card = app.descendants(matching: .any)["chat-queue"]
        XCTAssertTrue(card.waitForExistence(timeout: 5), "the queued message shows above the composer")
        XCTAssertTrue(card.staticTexts["跑完再看文档"].exists)
        XCTAssertTrue(app.buttons["chat-queued-steer"].exists && app.buttons["chat-queued-edit"].exists, "send now and edit")
        attach(app, "queued")
        app.buttons["chat-queued-remove"].tap()
        waitFor("queue emptied") { !card.exists }

        // Stop, then continue.
        stop.tap()
        let resume = app.buttons["chat-continue"]
        XCTAssertTrue(resume.waitForExistence(timeout: 5), "a stopped turn offers to continue")
        attach(app, "stopped")
        resume.tap()
        XCTAssertTrue(stop.waitForExistence(timeout: 5), "continuing starts a turn")
        stop.tap()
        XCTAssertTrue(resume.waitForExistence(timeout: 5))

        // Back to the list, then the new-chat form.
        app.buttons["chat-back"].tap()
        XCTAssertTrue(app.buttons["chat-session-fixture-1"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["chat-session-fixture-2"].exists)
        app.buttons["chat-new"].tap()
        XCTAssertTrue(app.navigationBars["新对话"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["chat-new-start"].isEnabled, "a usable agent and the home folder are preselected")
        attach(app, "new chat")
        app.buttons["chat-new-start"].tap()
        XCTAssertTrue(app.alerts["没能启动"].waitForExistence(timeout: 5), "a refused start says why")
        app.alerts["没能启动"].buttons.firstMatch.tap()
    }

    private func waitFor(_ what: String, timeout: TimeInterval = 5, _ condition: () -> Bool) {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return }
            RunLoop.current.run(until: Date().addingTimeInterval(0.1))
        }
        XCTFail("timed out waiting for \(what)")
    }

    private func attach(_ app: XCUIApplication, _ name: String) {
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = name; shot.lifetime = .keepAlways
        add(shot)
    }
}

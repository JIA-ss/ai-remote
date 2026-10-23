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
        XCTAssertTrue(app.staticTexts["读了 2 个文件，运行了 1 条命令 和 编辑了 1 个文件"].exists, "the run's sentence, edits included")
        XCTAssertTrue(app.staticTexts["· 1 个失败"].exists, "the run counts its failed call")
        XCTAssertTrue(app.staticTexts["完成 2/3"].exists, "the plan's progress")
        XCTAssertTrue(app.descendants(matching: .any)["chat-context"].exists, "the context ring")
        attach(app, "rows")

        app.buttons["chat-option-1"].tap()
        XCTAssertTrue(app.staticTexts["已回答：要"].waitForExistence(timeout: 5), "a numbered option answers the question")

        // The edit sits in the run: open the run, then its diff card.
        app.buttons.matching(identifier: "chat-run").firstMatch.tap()
        let diff = app.buttons["chat-diff"]
        XCTAssertTrue(diff.waitForExistence(timeout: 5))
        diff.tap()
        XCTAssertTrue(app.staticTexts["Run rustup target add aarch64-apple-ios aarch64-apple-ios-sim."].waitForExistence(timeout: 5), "the diff opens")
        attach(app, "diff")
        diff.tap()
        app.buttons.matching(identifier: "chat-run").firstMatch.tap()

        // Sending starts a turn; while it runs, a message joins it (the agent can steer), shown like any other.
        let input = app.textViews["chat-input"]
        input.tap()
        input.typeText("再跑一次测试")
        app.buttons["chat-send"].tap()
        XCTAssertTrue(app.staticTexts["再跑一次测试"].waitForExistence(timeout: 5))
        let stop = app.buttons["chat-stop"]
        XCTAssertTrue(stop.waitForExistence(timeout: 5), "a running turn shows the stop button")
        XCTAssertFalse(app.buttons["chat-mode"].exists, "no steer / queue switch")
        input.tap()
        input.typeText("用 release 配置")
        app.buttons["chat-send"].tap()
        XCTAssertTrue(app.staticTexts["用 release 配置"].waitForExistence(timeout: 5), "the message joins the running turn")
        XCTAssertFalse(app.descendants(matching: .any)["chat-queue"].exists, "it is not queued")
        XCTAssertFalse(app.staticTexts["插话"].exists, "and not flagged")
        attach(app, "steer")

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

    /// An agent that cannot steer: a message sent while it works waits above the composer.
    func testQueueWhenTheAgentCannotSteer() {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments = ["-ui-chat-fixture", "-ui-chat-no-steer"]
        app.launch()
        defer { app.terminate() }
        let input = app.textViews["chat-input"]
        XCTAssertTrue(input.waitForExistence(timeout: 15))
        input.tap()
        input.typeText("先跑测试")
        app.buttons["chat-send"].tap()
        XCTAssertTrue(app.buttons["chat-stop"].waitForExistence(timeout: 5))
        input.tap()
        input.typeText("跑完再看文档")
        app.buttons["chat-send"].tap()
        let card = app.descendants(matching: .any)["chat-queue"]
        XCTAssertTrue(card.waitForExistence(timeout: 5), "the queued message shows above the composer")
        XCTAssertTrue(card.staticTexts["跑完再看文档"].exists)
        XCTAssertTrue(app.buttons["chat-queued-edit"].exists, "edit")
        XCTAssertFalse(app.buttons["chat-queued-steer"].exists, "no separate steer")
        attach(app, "queued")
        app.buttons["chat-queued-remove"].tap()
        waitFor("queue emptied") { !card.exists }
    }

    /// Previews under a turn and in a sheet (image, page without scripts, Markdown, source), a link in
    /// a reply that opens one, the `/` and `@` menus, an image in the composer, a message the host
    /// refused and sent again, sessions by project, and the folder picker browsing the host.
    func testPreviewsComposerMenusImagesDeliveryProjectsAndFolders() {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments = ["-ui-chat-fixture", "-ui-chat-attach", "-ui-chat-unacked"]
        app.launch()
        defer { app.terminate() }

        // The second turn wrote a page and a chart and named a note: three cards under it.
        XCTAssertTrue(app.staticTexts["把构建结果整理成报告"].waitForExistence(timeout: 15))
        let page = app.buttons["chat-preview-report.html"], chart = app.buttons["chat-preview-chart.png"], note = app.buttons["chat-preview-notes.md"]
        XCTAssertTrue(page.waitForExistence(timeout: 5) && chart.exists && note.exists, "the turn's files show under it")
        shot(app, "01-previews-under-turn")

        chart.tap()
        XCTAssertTrue(app.descendants(matching: .any)["chat-preview-image"].waitForExistence(timeout: 5), "an image opens large")
        shot(app, "02-preview-image")
        app.buttons["chat-preview-done"].tap()

        page.tap()
        let web = app.webViews.firstMatch
        assertReportHTML(web)
        shot(app, "03-preview-html")
        app.segmentedControls.buttons["源码"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["chat-preview-source"].waitForExistence(timeout: 5), "the source view")
        shot(app, "04-preview-source")
        app.buttons["chat-preview-done"].tap()

        note.tap()
        XCTAssertTrue(app.staticTexts["构建说明"].waitForExistence(timeout: 5), "Markdown renders as a document")
        XCTAssertTrue(app.staticTexts["模拟器里不能拍照。"].exists, "with its quote")
        shot(app, "05-preview-markdown")
        app.buttons["chat-preview-done"].tap()

        // A link in a reply that names a file opens its preview.
        let link = app.links["报告"]
        XCTAssertTrue(link.waitForExistence(timeout: 3), "the reply exposes its file link")
        link.tap()
        assertReportHTML(app.webViews.firstMatch)
        app.buttons["chat-preview-done"].tap()

        // The composer: an image waiting to go, the / menu, the @ menu.
        XCTAssertTrue(app.descendants(matching: .any)["chat-attachments"].exists, "the attached image shows above the field")
        let input = app.textViews["chat-input"]
        input.tap()
        input.typeText("/re")
        let slash = app.descendants(matching: .any)["chat-slash-menu"]
        XCTAssertTrue(slash.waitForExistence(timeout: 5), "/ at the start opens the command menu")
        shot(app, "06-slash-menu")
        app.buttons["chat-ac-/review"].tap()
        waitFor("the command in the field") { (input.value as? String) == "/review " }
        input.typeText("@rep")
        let mention = app.descendants(matching: .any)["chat-mention-menu"]
        XCTAssertTrue(mention.waitForExistence(timeout: 5), "@ opens the file menu")
        XCTAssertTrue(app.buttons["chat-ac-report.html"].waitForExistence(timeout: 5), "with the matching file")
        shot(app, "07-mention-menu")
        app.buttons["chat-ac-report.html"].tap()
        waitFor("the path in the field") { (input.value as? String) == "/review @docs/report.html " }

        // Sent: the host refuses it (fixture), the bubble says so; sent again, it arrives with its image.
        app.buttons["chat-send"].tap()
        let retry = app.buttons["chat-retry"]
        XCTAssertTrue(retry.waitForExistence(timeout: 5), "a refused message offers to send it again")
        XCTAssertTrue(app.staticTexts["未送达：UI 测试：被控端拒收了这条消息"].exists, "and says why")
        shot(app, "08-not-delivered")
        retry.tap()
        waitFor("delivered") { !app.descendants(matching: .any)["chat-outgoing"].exists }
        XCTAssertTrue(app.staticTexts["/review @docs/report.html"].exists, "the message is in the conversation")
        XCTAssertTrue(app.descendants(matching: .any)["chat-user-images"].exists, "with this phone's picture of its image")
        shot(app, "09-delivered-with-image")
        app.buttons["chat-stop"].tap()

        // Sessions by project.
        app.buttons["chat-back"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["chat-project-ai-remote"].waitForExistence(timeout: 5), "a header per project")
        XCTAssertTrue(app.descendants(matching: .any)["chat-project-notes"].exists)
        XCTAssertTrue(app.buttons["chat-session-fixture-3"].exists, "two sessions in one project")
        shot(app, "10-sessions-by-project")

        // The folder picker: recent folders, then browsing the host's folders.
        app.buttons["chat-new"].tap()
        app.buttons["chat-new-cwd"].tap()
        XCTAssertTrue(app.buttons["chat-folder-recent-ai-remote"].waitForExistence(timeout: 5), "the sessions' folders are offered first")
        shot(app, "11-folder-recent")
        app.buttons["chat-folder-browse"].tap()
        XCTAssertTrue(app.buttons["chat-folder-child-work"].waitForExistence(timeout: 5), "the home folder's subfolders")
        app.buttons["chat-folder-child-work"].tap()
        let found = app.buttons["chat-folder-child-ai-remote"].waitForExistence(timeout: 5)
        if !found { shot(app, "12x-folder-work"); print(app.debugDescription) }
        XCTAssertTrue(found)
        app.buttons["chat-folder-child-ai-remote"].tap()
        XCTAssertTrue(app.buttons["chat-folder-child-crates"].waitForExistence(timeout: 5))
        shot(app, "12-folder-browse")
        app.buttons["chat-folder-use-bottom"].tap()
        let cwd = app.buttons["chat-new-cwd"]
        XCTAssertTrue(cwd.waitForExistence(timeout: 5))
        XCTAssertTrue(cwd.label.contains("ai-remote") && cwd.label.contains("~/work"), "the chosen folder, with its parent under it: \(cwd.label)")
        shot(app, "13-folder-chosen")
    }

    func testHTMLPreviewLoadsAgainFromItsCardLinkAndSourceView() {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments = ["-ui-chat-fixture"]
        app.launch()
        defer { app.terminate() }
        XCTAssertTrue(app.staticTexts["把构建结果整理成报告"].waitForExistence(timeout: 15))
        let card = app.buttons["chat-preview-report.html"]
        let link = app.links["报告"]
        for entry in [card, link, card] {
            XCTAssertTrue(entry.waitForExistence(timeout: 5), "the report can be reopened")
            entry.tap()
            assertReportHTML(app.webViews.firstMatch)
            app.segmentedControls.buttons["源码"].tap()
            XCTAssertTrue(app.descendants(matching: .any)["chat-preview-source"].waitForExistence(timeout: 5))
            app.segmentedControls.buttons["预览"].tap()
            assertReportHTML(app.webViews.firstMatch)
            app.buttons["chat-preview-done"].tap()
        }
    }

    private func assertReportHTML(_ web: XCUIElement, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertTrue(web.waitForExistence(timeout: 5), "a page opens in a web view", file: file, line: line)
        // A cold iOS 26.2 CI simulator took 34.8 s to launch WebContent (run 39954997454).
        // Wait for the document, rather than treating the web view's existence as rendered HTML.
        XCTAssertTrue(web.staticTexts["构建报告"].waitForExistence(timeout: 45), "its HTML and CSS render", file: file, line: line)
        XCTAssertTrue(web.staticTexts["脚本没有运行"].exists, "its script does not run", file: file, line: line)
    }

    private func waitFor(_ what: String, timeout: TimeInterval = 5, _ condition: () -> Bool) {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return }
            RunLoop.current.run(until: Date().addingTimeInterval(0.1))
        }
        XCTFail("timed out waiting for \(what)")
    }

    /// A screenshot kept with the result and written to /tmp/ios-chat-v3 on the Mac (the simulator shares its file system).
    private func shot(_ app: XCUIApplication, _ name: String) {
        attach(app, name)
        let dir = URL(fileURLWithPath: "/tmp/ios-chat-v3")
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        try? app.screenshot().pngRepresentation.write(to: dir.appendingPathComponent(name + ".png"))
    }

    private func attach(_ app: XCUIApplication, _ name: String) {
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = name; shot.lifetime = .keepAlways
        add(shot)
    }
}

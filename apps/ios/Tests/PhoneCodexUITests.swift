import XCTest
import AIRemotePhone

/// The Codex view over the `-ui-chat-fixture -ui-chat-codex` demo session (no host): a finished turn
/// folded behind "Worked for …" that opens to its work, a group that opens to its lines, a command that
/// opens to its shell, an edit and a changed file that open to their diffs; and a live turn
/// (`-ui-codex-live`) that says what runs now and how long it has worked, then stops and offers to
/// continue. `TEST_RUNNER_CODEX_LANG=zh` runs it in Chinese; screenshots go to /tmp/ios-codex on the
/// Mac with the suffix in `TEST_RUNNER_CODEX_SHOTS` (e.g. `light`, `dark`).
final class PhoneCodexUITests: XCTestCase {
    private var lang: PhoneCodexLang { ProcessInfo.processInfo.environment["CODEX_LANG"] == "zh" ? .zh : .en }
    private func words(_ key: String, _ vars: [String: String] = [:]) -> String { phoneCodexText(key, vars, lang: lang) }

    private func launch(_ extra: [String] = []) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = ["-ui-chat-fixture", "-ui-chat-codex"] + extra
            + (lang == .zh ? ["-AppleLanguages", "(zh-Hans)", "-AppleLocale", "zh_CN"] : ["-AppleLanguages", "(en)", "-AppleLocale", "en_US"])
        app.launch()
        return app
    }

    func testCodexTurnFoldsAndOpensToGroupsShellsAndDiffs() {
        continueAfterFailure = false
        let app = launch()
        defer { app.terminate() }

        // The conversation opens at its end: the second turn, then the first one's changes above it.
        XCTAssertTrue(app.staticTexts["Does monthrange handle leap years?"].waitForExistence(timeout: 15))
        let foot = app.descendants(matching: .any)["codex-foot-2"]
        XCTAssertTrue(foot.exists, "a turn without work to fold says how long it worked under its answer")
        XCTAssertTrue(foot.staticTexts[words("cdx_worked_for", ["t": phoneCodexDuration(6_100, zh: lang == .zh)])].exists, "worked for 6s")
        let file = app.buttons["codex-file-export.py"]
        XCTAssertTrue(scroll(app, to: file, up: true), "the first turn's changes card")
        XCTAssertTrue(app.staticTexts[phoneCodexPlural("cdx_files_changed", 2, lang: lang) + "  +12 −2"].exists
                      || app.descendants(matching: .any)["codex-changes"].exists, "2 files changed")

        // The first turn: its work folded behind "Worked for 1m 23s" over its answer.
        let worked = app.buttons["codex-worked"]
        XCTAssertTrue(scroll(app, to: worked, up: true), "the fold")
        XCTAssertTrue(worked.label.contains(words("cdx_worked_for", ["t": phoneCodexDuration(83_400, zh: lang == .zh)])), worked.label)
        XCTAssertFalse(app.buttons["codex-group-cc1"].exists, "the work is folded")
        XCTAssertTrue(app.staticTexts["The Q3 export crashes with a date error. Find the cause, fix it and add a test."].exists, "the user's message stays")
        shot(app, "c01-turn-folded")

        // Open: the remarks and two groups of tool calls (a plan update does not split the first).
        worked.tap()
        let group = app.buttons["codex-group-cc1"]
        XCTAssertTrue(group.waitForExistence(timeout: 5), "the fold opens to the work")
        XCTAssertTrue(app.buttons["codex-group-cf1"].exists, "a second group after the next remark (the plan update did not split the first)")
        let readAndRan = phoneCodexPlural("cdx_s_read_lead", 1, lang: lang)
        XCTAssertTrue(group.label.hasPrefix(readAndRan), "the first group's sentence: \(group.label)")
        XCTAssertTrue(app.staticTexts["I'll find where the quarter dates are computed, then reproduce the crash."].exists, "a remark between steps")
        shot(app, "c02-work-open")

        // A group opens to its lines; a failed command opens to its shell with the exit code.
        group.tap()
        let run = app.buttons["codex-act-cc4"]
        XCTAssertTrue(run.waitForExistence(timeout: 5), "the group opens to its lines")
        XCTAssertTrue(app.buttons["codex-act-cc1"].exists && app.buttons["codex-act-cc2"].exists && app.buttons["codex-act-cc3"].exists, "one line per call")
        XCTAssertTrue(app.buttons["codex-act-cc2"].label.hasPrefix(words("cdx_v_read") + " export.py"), app.buttons["codex-act-cc2"].label)
        XCTAssertTrue(run.label.contains("python export.py --quarter Q3"), run.label)
        run.tap()
        let shell = app.descendants(matching: .any)["codex-shell"]
        XCTAssertTrue(shell.waitForExistence(timeout: 5), "the command opens to its shell")
        bringIntoView(app, shell)
        XCTAssertTrue(shell.staticTexts[words("cdx_exit_code", ["n": "1"])].exists, "with its exit code")
        XCTAssertTrue(text(shell, "ValueError: day is out of range for month").exists, "and its output")
        shot(app, "c03-group-shell-open")
        run.tap()
        group.tap()
        waitFor("the group closes") { !app.buttons["codex-act-cc4"].exists }

        // The second group: the edit's files, each opening to its diff with line numbers.
        let second = app.buttons["codex-group-cf1"]
        XCTAssertTrue(scroll(app, to: second, up: false))
        second.tap()
        let edit = app.buttons["codex-act-cf1-export.py"]
        XCTAssertTrue(edit.waitForExistence(timeout: 5), "an edit of two files has a line per file")
        XCTAssertTrue(app.buttons["codex-act-cf1-test_quarters.py"].exists)
        edit.tap()
        let diff = app.descendants(matching: .any)["codex-diff"]
        XCTAssertTrue(diff.waitForExistence(timeout: 5), "the file opens to its diff")
        bringIntoView(app, diff)
        XCTAssertTrue(text(diff, "last = calendar.monthrange(year, quarter * 3)[1]").exists, "an added line")
        XCTAssertTrue(diff.staticTexts["from datetime import date"].exists, "the hunk's place, as git names it")
        shot(app, "c04-edit-diff-open")
        edit.tap()

        // The turn's changes: a changed file opens to its diff.
        XCTAssertTrue(scroll(app, to: file, up: false))
        file.tap()
        let changed = app.descendants(matching: .any)["codex-changes"]
        XCTAssertTrue(changed.descendants(matching: .any)["codex-diff"].waitForExistence(timeout: 5), "a changed file opens to its diff")
        bringIntoView(app, changed)
        XCTAssertTrue(text(changed, "end = date(year, quarter * 3, 31)").exists, "a removed line")
        shot(app, "c05-changes-open")

        // Folding again hides the work.
        XCTAssertTrue(scroll(app, to: worked, up: true))
        worked.tap()
        waitFor("folded again") { !app.buttons["codex-group-cc1"].exists }
    }

    func testLiveCodexTurnSaysWhatRunsThenStops() {
        continueAfterFailure = false
        let app = launch(["-ui-codex-live"])
        defer { app.terminate() }
        let stop = app.buttons["chat-stop"]
        XCTAssertTrue(stop.waitForExistence(timeout: 15), "the turn works")
        let status = app.descendants(matching: .any)["codex-status"]
        XCTAssertTrue(status.waitForExistence(timeout: 5), "how long it has worked")
        let group = app.buttons["codex-group-a3"]
        XCTAssertTrue(group.exists, "its tool calls in one live line")
        XCTAssertTrue(group.label.hasPrefix(words("cdx_v_running") + " python -m pytest -q tests/test_monthly.py"), "the line names what runs now: \(group.label)")
        XCTAssertTrue(app.buttons["codex-plan"].exists, "the plan above the work, open while the turn runs")
        XCTAssertTrue(app.staticTexts["Fix report/monthly.py"].exists, "its steps")
        waitFor("a second passed", timeout: 4) { status.label.contains(self.words("cdx_working_for", ["t": ""]).trimmingCharacters(in: .whitespaces)) }
        shot(app, "c06-live-turn")
        group.tap()
        XCTAssertTrue(app.buttons["codex-act-c3"].waitForExistence(timeout: 5), "the live group opens to its lines")
        XCTAssertTrue(app.buttons["codex-act-w3"].exists && app.buttons["codex-act-b3"].exists, "the web search and the read are in it too")
        bringIntoView(app, app.buttons["codex-act-c3"])
        shot(app, "c07-live-group-open")

        stop.tap()
        let resume = app.buttons["chat-continue"]
        XCTAssertTrue(resume.waitForExistence(timeout: 5), "a stopped turn offers to continue")
        XCTAssertTrue(app.descendants(matching: .any)["codex-stopped"].exists, "and says it was stopped")
        waitFor("no longer working") { !app.descendants(matching: .any)["codex-status"].exists }
        shot(app, "c08-stopped")
    }

    /// Swipes the transcript up until `element`'s bottom is above the composer.
    private func bringIntoView(_ app: XCUIApplication, _ element: XCUIElement) {
        let window = app.windows.firstMatch
        for _ in 0..<8 where element.exists && element.frame.maxY > window.frame.maxY * 0.78 {
            let from = window.coordinate(withNormalizedOffset: CGVector(dx: 0.45, dy: 0.7))
            from.press(forDuration: 0.05, thenDragTo: window.coordinate(withNormalizedOffset: CGVector(dx: 0.45, dy: 0.5)))
        }
    }

    /// A text in `element` that contains `s`.
    private func text(_ element: XCUIElement, _ s: String) -> XCUIElement {
        element.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", s)).firstMatch
    }

    /// Swipes until `element` is on screen, clear of the navigation bar and the composer (the transcript
    /// builds its rows lazily, so one that is far away does not exist until the list gets near it).
    private func scroll(_ app: XCUIApplication, to element: XCUIElement, up: Bool, tries: Int = 16) -> Bool {
        let window = app.windows.firstMatch
        func drag(_ from: CGFloat, _ to: CGFloat) {
            window.coordinate(withNormalizedOffset: CGVector(dx: 0.45, dy: from))
                .press(forDuration: 0.05, thenDragTo: window.coordinate(withNormalizedOffset: CGVector(dx: 0.45, dy: to)))
        }
        for _ in 0..<tries {
            let h = window.frame.height
            if !element.exists {
                up ? drag(0.35, 0.7) : drag(0.7, 0.35)
            } else if element.frame.minY < h * 0.18 {
                drag(0.4, 0.55)
            } else if element.frame.maxY > h * 0.75 {
                drag(0.55, 0.4)
            } else {
                return element.isHittable
            }
        }
        return element.exists && element.isHittable
    }

    private func waitFor(_ what: String, timeout: TimeInterval = 5, _ condition: () -> Bool) {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return }
            RunLoop.current.run(until: Date().addingTimeInterval(0.1))
        }
        XCTFail("timed out waiting for \(what)")
    }

    /// A screenshot kept with the result and written to /tmp/ios-codex on the Mac (the simulator shares its file system).
    private func shot(_ app: XCUIApplication, _ name: String) {
        let suffix = ProcessInfo.processInfo.environment["CODEX_SHOTS"].map { "-\($0)" } ?? ""
        let image = app.screenshot()
        let attachment = XCTAttachment(screenshot: image)
        attachment.name = name; attachment.lifetime = .keepAlways
        add(attachment)
        let dir = URL(fileURLWithPath: "/tmp/ios-codex")
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        try? image.pngRepresentation.write(to: dir.appendingPathComponent(name + suffix + ".png"))
    }
}

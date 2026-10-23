import XCTest
import AIRemotePhone

/// The Codex view over the `-ui-chat-fixture -ui-chat-codex` demo session (no host): a finished turn
/// whose work folds behind "Worked for …" at its top and opens to its groups, a command that opens to
/// its shell, an edit and the turn's changes card that open to their diffs (Review opens them all); a
/// live turn (`-ui-codex-live`) with "Working for …" over its work, a line naming what runs now and its
/// plan as a pill above the composer, which then stops and offers to continue; and a turn that waits on
/// two questions (`-ui-codex-ask`): a card in place of the composer, one question at a time, answered
/// (or closed, which stops the turn) and left as one "Asked 2 questions" line, the composer's draft
/// intact. `TEST_RUNNER_CODEX_LANG=zh` runs it in Chinese; screenshots go to /tmp/ios-codex2 on the Mac
/// with the suffix in `TEST_RUNNER_CODEX_SHOTS` (e.g. `light`, `dark`).
final class PhoneCodexUITests: XCTestCase {
    private var lang: PhoneCodexLang { ProcessInfo.processInfo.environment["CODEX_LANG"] == "zh" ? .zh : .en }
    private func words(_ key: String, _ vars: [String: String] = [:]) -> String { phoneCodexText(key, vars, lang: lang) }
    /// What a template says before its `{t}`.
    private func lead(_ key: String) -> String { words(key, ["t": "\u{1}"]).components(separatedBy: "\u{1}")[0].trimmingCharacters(in: .whitespaces) }

    private func launch(_ extra: [String] = []) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = ["-ui-chat-fixture", "-ui-chat-codex"] + extra
            + (lang == .zh ? ["-AppleLanguages", "(zh-Hans)", "-AppleLocale", "zh_CN"] : ["-AppleLanguages", "(en)", "-AppleLocale", "en_US"])
        app.launch()
        return app
    }

    func testCodexTurnFoldsAndOpensToGroupsShellsAndDiffs() throws {
        continueAfterFailure = false
        let app = launch()
        defer { app.terminate() }

        // The conversation opens at its end: the second turn (no work, so no timer over it), then the
        // first one's changes above it.
        XCTAssertTrue(app.staticTexts["Does monthrange handle leap years?"].waitForExistence(timeout: 15))
        let foot = app.descendants(matching: .any)["codex-foot-2"]
        XCTAssertTrue(foot.exists, "under the answer: copy and when it came")
        XCTAssertTrue(foot.buttons[words("cdx_copy_response")].exists, "copy")
        XCTAssertFalse(foot.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "tokens")).firstMatch.exists, "no token count")
        let title = app.descendants(matching: .any)["codex-changes-title"]
        XCTAssertTrue(scroll(app, to: title, up: true), "the first turn's changes card")
        XCTAssertTrue(title.label.contains(words("cdx_edited_files", ["n": "2"])), title.label)

        // The first turn: "Worked for …" at the top of its work, which is folded behind it; the time is
        // up to the answer's start (18 events of 3.3 s apart in the fixture), not the turn's end.
        let worked = app.buttons["codex-worked"]
        XCTAssertTrue(scroll(app, to: worked, up: true), "the fold")
        XCTAssertEqual(worked.label, words("cdx_worked_for", ["t": phoneDurationB(18 * 3300, lang: lang)]))
        XCTAssertFalse(app.buttons["codex-group-cc1"].exists, "the work is folded")
        let user = app.staticTexts["The Q3 export crashes with a date error. Find the cause, fix it and add a test."]
        XCTAssertTrue(user.exists && user.frame.maxY < worked.frame.minY, "the timer is under the user's message")
        shot(app, "c01-turn-folded")

        // Open: the remarks and two groups of tool calls (a plan update does not split the first);
        // reasoning is not a line of its own.
        worked.tap()
        let group = app.buttons["codex-group-cc1"]
        XCTAssertTrue(group.waitForExistence(timeout: 5), "the fold opens to the work")
        XCTAssertTrue(group.frame.minY > worked.frame.maxY, "the work is under the timer")
        XCTAssertTrue(app.buttons["codex-group-cf1"].exists, "a second group after the next remark (the plan update did not split the first)")
        XCTAssertTrue(group.label.hasPrefix(phoneCodexPlural("cdx_s_read_lead", 1, lang: lang)), "the first group's sentence: \(group.label)")
        XCTAssertTrue(app.staticTexts["I'll find where the quarter dates are computed, then reproduce the crash."].exists, "a remark between steps")
        XCTAssertFalse(app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "Locating the date math")).firstMatch.exists, "no reasoning row")
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
        second.tap()

        // The turn's changes: a changed file opens to its diff; Review opens every one.
        let file = app.buttons["codex-file-export.py"]
        XCTAssertTrue(scroll(app, to: file, up: false))
        file.tap()
        let changed = app.descendants(matching: .any)["codex-changes"]
        XCTAssertTrue(changed.descendants(matching: .any)["codex-diff"].waitForExistence(timeout: 5), "a changed file opens to its diff")
        bringIntoView(app, changed)
        XCTAssertTrue(text(changed, "end = date(year, quarter * 3, 31)").exists, "a removed line")
        // Exercise the expanded card through the audit engine used by Accessibility Inspector too.
        // This is an automated audit, not a VoiceOver navigation test.
        try app.performAccessibilityAudit(for: [.elementDetection])
        file.tap()
        waitFor("closed again") { changed.descendants(matching: .any).matching(identifier: "codex-diff").count == 0 }
        let review = changed.buttons["codex-review"]
        XCTAssertTrue(scroll(app, to: review, up: true))
        review.tap()
        waitFor("Review opens both files") { changed.descendants(matching: .any).matching(identifier: "codex-diff").count == 2 }
        try app.performAccessibilityAudit(for: [.elementDetection])
        shot(app, "c05-changes-review")

        // Folding again hides the work.
        XCTAssertTrue(scroll(app, to: worked, up: true))
        worked.tap()
        waitFor("folded again") { !app.buttons["codex-group-cc1"].exists }
    }

    func testLongCodexHistoryStartsWithThirtyTurnsAndOpensTheEarlierPage() {
        continueAfterFailure = false
        let app = launch(["-ui-codex-history"])
        defer { app.terminate() }
        XCTAssertTrue(app.staticTexts["History request 35"].waitForExistence(timeout: 15), "the conversation starts at its end")
        let turns = app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH %@", "codex-turn-"))
        XCTAssertEqual(turns.count, 30, "only the latest page is mounted")
        XCTAssertTrue(app.staticTexts["History request 6"].exists)
        XCTAssertFalse(app.staticTexts["History request 1"].exists, "older turns are not mounted yet")
        let earlier = app.buttons["chat-earlier-turns"]
        XCTAssertTrue(scroll(app, to: earlier, up: true, tries: 40), "the history button is above the visible turns")
        XCTAssertEqual(earlier.label, words("cdx_show_earlier_turns"))
        earlier.tap()
        let first = app.staticTexts["History request 1"]
        XCTAssertTrue(first.waitForExistence(timeout: 5), "the older page joins the existing turns")
        XCTAssertEqual(turns.count, 35, "no previous turns were dropped or duplicated")
        XCTAssertFalse(earlier.exists, "there are no more retained turns to open")
        XCTAssertTrue(first.isHittable, "opening history positions its new first turn in view")
        let second = app.staticTexts["History request 2"]
        XCTAssertTrue(first.frame.minY < second.frame.minY, "older turns stay in chronological order")
        XCTAssertTrue(app.staticTexts["History request 35"].exists, "the latest turn is still mounted")
        shot(app, "c12-earlier-history")
    }

    func testLiveCodexTurnSaysWhatRunsAndShowsItsPlanThenStops() {
        continueAfterFailure = false
        let app = launch(["-ui-codex-live"])
        defer { app.terminate() }
        let stop = app.buttons["chat-stop"]
        XCTAssertTrue(stop.waitForExistence(timeout: 15), "the turn works")
        let timer = inTurn(app, 3)["codex-worked"]
        XCTAssertTrue(timer.waitForExistence(timeout: 5), "the timer over the work")
        waitFor("a second passed", timeout: 4) { timer.label.hasPrefix(self.lead("cdx_working_for")) }
        let group = app.buttons["codex-group-a3"]
        XCTAssertTrue(group.exists, "its tool calls in one live line")
        XCTAssertTrue(timer.frame.maxY <= group.frame.minY, "the timer is at the top of the work")
        XCTAssertTrue(group.label.hasPrefix(words("cdx_v_running") + " python -m pytest -q tests/test_monthly.py"), "the line names what runs now: \(group.label)")

        // The plan is a pill above the composer, not a card in the transcript; a tap lists its steps.
        let pill = app.buttons["codex-plan"]
        XCTAssertTrue(pill.exists, "the plan above the composer")
        XCTAssertTrue(pill.label.contains(words("cdx_plan_step", ["s": "2", "n": "3"])), pill.label)
        XCTAssertTrue(pill.frame.minY < app.descendants(matching: .any)["chat-input"].frame.minY, "above the composer")
        XCTAssertFalse(app.staticTexts["Fix report/monthly.py"].exists, "no plan in the transcript")
        shot(app, "c06-live-turn")
        pill.tap()
        let steps = app.descendants(matching: .any)["codex-plan-steps"]
        XCTAssertTrue(steps.waitForExistence(timeout: 5), "the steps")
        XCTAssertTrue(text(steps, "Fix report/monthly.py").exists && text(steps, "Test February in 2024 and 2026").exists, "every step")
        shot(app, "c07-plan-steps")
        app.windows.firstMatch.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.2)).tap()
        waitFor("the steps close") { !app.descendants(matching: .any)["codex-plan-steps"].exists }

        group.tap()
        XCTAssertTrue(app.buttons["codex-act-c3"].waitForExistence(timeout: 5), "the live group opens to its lines")
        XCTAssertTrue(app.buttons["codex-act-w3"].exists && app.buttons["codex-act-b3"].exists, "the web search and the read are in it too")
        XCTAssertTrue(app.buttons["codex-act-w3"].label.contains("python calendar.monthrange leap year"), app.buttons["codex-act-w3"].label)
        bringIntoView(app, app.buttons["codex-act-c3"])

        stop.tap()
        let resume = app.buttons["chat-continue"]
        XCTAssertTrue(resume.waitForExistence(timeout: 5), "a stopped turn offers to continue")
        waitFor("the timer says it was stopped") { self.inTurn(app, 3)["codex-worked"].label.hasPrefix(self.lead("cdx_stopped_after")) }
        XCTAssertFalse(app.buttons["codex-plan"].exists, "no plan once the turn ended")
        shot(app, "c08-stopped")
    }

    /// The fixture's request, named as a host names it (process start and Codex's id).
    private let request = "1761206400000-3"

    func testCodexQuestionCardAsksOneAtATimeThenLeavesAnAskedLine() {
        continueAfterFailure = false
        let app = launch(["-ui-codex-ask"])
        defer { app.terminate() }
        let card = app.descendants(matching: .any)["codex-request"]
        XCTAssertTrue(card.waitForExistence(timeout: 15), "the question card")
        XCTAssertFalse(app.descendants(matching: .any)["chat-input"].exists, "in place of the composer")
        XCTAssertEqual(card.staticTexts["codex-request-prompt"].label, "report/monthly.py builds month ends by hand too. Fix it in this change?")
        XCTAssertEqual(card.staticTexts["codex-request-page"].label, words("cdx_q_page", ["i": "1", "n": "2"]))
        let first = card.buttons["codex-option-1"]
        XCTAssertTrue(first.label.contains("Fix both") && !first.label.contains("(Recommended)"), first.label)
        XCTAssertTrue(first.label.contains(words("cdx_q_recommended")), "the mark is a badge: \(first.label)")
        XCTAssertTrue(first.label.contains("Same bug: day 30 is wrong for 31-day months"), "with its description")
        XCTAssertTrue(first.isSelected, "the first option is chosen from the start")
        XCTAssertTrue(card.textFields["codex-request-field"].exists || card.textViews["codex-request-field"].exists, "a row for an answer of one's own")
        let status = app.descendants(matching: .any)["codex-status"]
        XCTAssertTrue(status.exists && status.label.contains(words("cdx_waiting_answer")), "the turn says it waits")
        XCTAssertFalse(app.descendants(matching: .any)["codex-asked-\(request)"].exists, "a waiting question is not in the transcript")
        let expires = card.staticTexts["codex-request-expires"]
        XCTAssertTrue(expires.exists && expires.label.contains(words("cdx_q_expires", ["t": "1:5"]).components(separatedBy: "1:5")[0].trimmingCharacters(in: .whitespaces)),
                      "Codex goes on by itself: \(expires.label)")
        XCTAssertTrue(expires.label.contains("1:5") || expires.label.contains("2:00"), "counting down from two minutes: \(expires.label)")
        shot(app, "c09-ask-card")

        // Another option moves on to the second question.
        card.buttons["codex-option-2"].tap()
        waitFor("the second question") { card.staticTexts["codex-request-page"].label == self.words("cdx_q_page", ["i": "2", "n": "2"]) }
        XCTAssertEqual(card.staticTexts["codex-request-prompt"].label, "Which years should the new tests cover?")
        let action = card.buttons["codex-request-action"]
        XCTAssertEqual(action.label, words("cdx_q_skip"))
        shot(app, "c10-ask-second")

        // Skipped: the answers go; the card leaves, the composer comes back with its draft.
        action.tap()
        waitFor("the card leaves") { !app.descendants(matching: .any)["codex-request"].exists }
        let input = app.descendants(matching: .any)["chat-input"]
        XCTAssertTrue(input.waitForExistence(timeout: 5), "the composer is back")
        XCTAssertEqual(input.value as? String, "Also check the weekly report.", "with its draft")
        waitFor("the composer has the keyboard back") { app.keyboards.firstMatch.exists }
        let asked = app.buttons["codex-asked-\(request)"]
        XCTAssertTrue(asked.waitForExistence(timeout: 5), "one line for the request")
        XCTAssertTrue(asked.label.hasPrefix(words("cdx_q_asked") + " " + phoneCodexPlural("cdx_q_n", 2, lang: lang)), asked.label)
        asked.tap()
        let scope = app.descendants(matching: .any)["codex-asked-scope"]
        XCTAssertTrue(scope.waitForExistence(timeout: 5))
        XCTAssertTrue(scope.label.contains("Only the quarterly export"), scope.label)
        XCTAssertTrue(app.descendants(matching: .any)["codex-asked-years"].label.contains(words("cdx_q_no_answer")), "the skipped one")
        XCTAssertTrue(app.descendants(matching: .any)["codex-thinking"].label.contains("Fixing both month ends"), "the turn goes on, saying what it is at")
        shot(app, "c11-asked")
    }

    func testClosingTheQuestionCardStopsTheTurn() {
        continueAfterFailure = false
        let app = launch(["-ui-codex-ask"])
        defer { app.terminate() }
        let close = app.buttons["codex-request-close"]
        XCTAssertTrue(close.waitForExistence(timeout: 15))
        close.tap()
        XCTAssertTrue(app.buttons["chat-continue"].waitForExistence(timeout: 5), "the turn stopped")
        XCTAssertFalse(app.descendants(matching: .any)["codex-request"].exists, "the card is gone")
        XCTAssertTrue(inTurn(app, 3)["codex-worked"].label.hasPrefix(lead("cdx_stopped_after")), "the timer says so")
        XCTAssertTrue(app.buttons["codex-asked-\(request)"].exists, "the questions stay as one line")
        XCTAssertEqual(app.descendants(matching: .any)["chat-input"].value as? String, "Also check the weekly report.", "the draft is intact")
    }

    /// Everything in turn `n` (several turns have a timer of their own).
    private func inTurn(_ app: XCUIApplication, _ n: Int) -> XCUIElementQuery {
        app.descendants(matching: .any)["codex-turn-\(n)"].descendants(matching: .any)
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

    /// A screenshot kept with the result and written to /tmp/ios-codex2 on the Mac (the simulator shares its file system).
    private func shot(_ app: XCUIApplication, _ name: String) {
        let suffix = ProcessInfo.processInfo.environment["CODEX_SHOTS"].map { "-\($0)" } ?? ""
        let image = app.screenshot()
        let attachment = XCTAttachment(screenshot: image)
        attachment.name = name; attachment.lifetime = .keepAlways
        add(attachment)
        let dir = URL(fileURLWithPath: "/tmp/ios-codex2")
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        try? image.pngRepresentation.write(to: dir.appendingPathComponent(name + suffix + ".png"))
    }
}

import XCTest
import CoreGraphics

final class PhoneUITests: XCTestCase {
    func testSessionViewportFullscreenRotationZoomAndKeyboard() {
        continueAfterFailure = false
        XCUIDevice.shared.orientation = .portrait
        let app = XCUIApplication()
        app.launchArguments = ["-ui-viewport-fixture"]
        app.launch()
        defer { XCUIDevice.shared.orientation = .portrait; app.terminate() }
        let viewport = app.descendants(matching: .any)["remote-viewport"]
        XCTAssertTrue(viewport.waitForExistence(timeout: 15))
        waitFor("portrait image fit") {
            let value = self.viewportMetrics(viewport)
            return (value["viewport_height"] ?? 0) > (value["viewport_width"] ?? 0) && (value["fit"] ?? 0) > 0
        }
        let input = app.staticTexts["fixture-remote-input"]
        XCTAssertEqual(input.value as? String, "none")
        // A touch in the fitted image's black margin must never become an edge click.
        viewport.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.06)).tap()
        XCTAssertEqual(input.value as? String, "none")
        let portrait = viewportMetrics(viewport)
        XCTAssertGreaterThan(portrait["displayed_width"] ?? 0, (portrait["available_width"] ?? 1) * 0.95)
        XCTAssertLessThan(portrait["displayed_height"] ?? 0, viewport.frame.height * 0.5)
        assertPointerMapping(app: app, viewport: viewport, start: CGPoint(x: 0.5, y: 0.5))

        let fullscreen = app.buttons["session-fullscreen"]
        let beforeFullscreenInput = fixtureSequence(input)
        fullscreen.tap()
        waitFor("fullscreen action state: \(fullscreen.value ?? "missing")") { fullscreen.label == "退出全屏" }
        XCTAssertEqual(fixtureSequence(input), beforeFullscreenInput, "The local fullscreen button must not emit a remote click")
        XCUIDevice.shared.orientation = .landscapeLeft
        waitFor("fullscreen state after rotation: \(fullscreen.value ?? "missing")") { fullscreen.label == "退出全屏" }
        XCTAssertEqual(fixtureSequence(input), beforeFullscreenInput)
        waitFor("landscape full picture area") {
            let value = self.viewportMetrics(viewport)
            let window = app.windows.firstMatch.frame
            return window.width > window.height && (value["viewport_width"] ?? 0) >= window.width * 0.95
                && (value["viewport_height"] ?? 0) >= window.height * 0.95
                && (value["available_width"] ?? 0) > 0 && (value["available_height"] ?? 0) > 0
                && (value["displayed_width"] ?? 0) >= (value["available_width"] ?? 1) * 0.9
                && (value["displayed_height"] ?? 0) >= (value["available_height"] ?? 1) * 0.8
        }
        assertCompleteFittedPicture(viewport)
        XCTAssertTrue(app.buttons["session-fullscreen"].isHittable)
        XCTAssertTrue(app.buttons["session-agent"].isHittable)
        XCTAssertTrue(app.buttons["session-take-over"].isHittable)
        attach(app, name: "Production session fixture - landscape fullscreen fitted 21 by 9")

        let beforeZoomInput = input.value as? String
        viewport.pinch(withScale: 2, velocity: 2)
        waitFor("native pinch zoom") {
            let value = self.viewportMetrics(viewport)
            return (value["zoom"] ?? 0) > (value["fit"] ?? 1) * 1.4
        }
        XCTAssertEqual(input.value as? String, beforeZoomInput, "Two-finger zoom must not send remote input")
        viewport.twoFingerTap()
        XCTAssertEqual(input.value as? String, beforeZoomInput, "Two-finger gestures must not become clicks")
        assertPointerMapping(app: app, viewport: viewport, start: CGPoint(x: 0.5, y: 0.5))
        assertPointerMapping(app: app, viewport: viewport, start: CGPoint(x: 0.4, y: 0.5), end: CGPoint(x: 0.6, y: 0.5))
        attach(app, name: "Production session fixture - zoomed native viewport")

        let beforeFitInput = fixtureSequence(input)
        app.buttons["session-fit"].tap()
        waitFor("fit restored") {
            let value = self.viewportMetrics(viewport)
            return abs((value["zoom"] ?? 0) - (value["fit"] ?? 1)) < 0.001
        }
        XCTAssertEqual(fixtureSequence(input), beforeFitInput, "Local fit must not emit remote input")
        XCTAssertFalse(app.textFields["session-text"].exists, "Keyboard and shortcuts must start collapsed")
        let controls = app.buttons["session-controls-toggle"]
        let beforeControlsInput = fixtureSequence(input)
        controls.tap()
        waitFor("controls collapsed") { controls.label == "展开控制栏" && !app.buttons["session-keyboard"].exists }
        XCTAssertEqual(fixtureSequence(input), beforeControlsInput, "A local toolbar action must not emit remote input")
        XCTAssertTrue(app.buttons["session-agent"].isHittable)
        XCTAssertTrue(app.buttons["session-take-over"].isHittable)
        controls.tap()
        waitFor("controls expanded") { controls.label == "收起控制栏" && app.buttons["session-keyboard"].isHittable }
        XCTAssertEqual(fixtureSequence(input), beforeControlsInput)
        let beforeKeyboardInput = fixtureSequence(input)
        app.buttons["session-keyboard"].tap()
        let field = app.textFields["session-text"]
        XCTAssertTrue(field.waitForExistence(timeout: 5))
        XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: 10))
        XCTAssertEqual(fixtureSequence(input), beforeKeyboardInput, "Opening a local keyboard must not emit remote input")
        field.typeText("UI-only draft")
        let close = app.buttons["session-keyboard-close"]
        XCTAssertTrue(close.isHittable)
        XCTAssertLessThanOrEqual(close.frame.maxY, app.keyboards.firstMatch.frame.minY)
        attach(app, name: "Production session fixture - explicit keyboard drawer")
        let beforeCloseInput = fixtureSequence(input)
        close.tap()
        waitFor("keyboard drawer closed") { !app.keyboards.firstMatch.exists && !field.exists }
        XCTAssertEqual(fixtureSequence(input), beforeCloseInput, "Closing a local keyboard must not emit remote input")
        waitFor("fullscreen picture recovered after keyboard") {
            let value = self.viewportMetrics(viewport)
            return (value["displayed_width"] ?? 0) >= (value["available_width"] ?? 1) * 0.9
                && (value["displayed_height"] ?? 0) >= (value["available_height"] ?? 1) * 0.8
        }
        XCUIDevice.shared.orientation = .portrait
        waitFor("portrait restored without changing the session") {
            let value = self.viewportMetrics(viewport)
            return (value["viewport_height"] ?? 0) > (value["viewport_width"] ?? 0)
        }
        app.buttons["session-fit"].tap()
        assertCompleteFittedPicture(viewport)
        assertPointerMapping(app: app, viewport: viewport, start: CGPoint(x: 0.5, y: 0.5))
        attach(app, name: "Production session fixture - portrait fullscreen fit")
    }

    func testUltrawideFillActuallyFillsPortraitViewport() {
        continueAfterFailure = false
        XCUIDevice.shared.orientation = .portrait
        let app = XCUIApplication()
        app.launchArguments = ["-ui-viewport-fixture", "-ui-viewport-ultrawide"]
        app.launch()
        defer { app.terminate() }
        let viewport = app.descendants(matching: .any)["remote-viewport"]
        XCTAssertTrue(viewport.waitForExistence(timeout: 15))
        waitFor("actual ultrawide fixture pixels") { self.viewportMetrics(viewport)["image_height"] == 540 }
        let fullscreen = app.buttons["session-fullscreen"]
        fullscreen.tap()
        waitFor("ultrawide fullscreen action state: \(fullscreen.value ?? "missing")") { fullscreen.label == "退出全屏" }
        XCTAssertEqual(fixtureSequence(app.staticTexts["fixture-remote-input"]), 0)
        app.buttons["session-menu"].tap()
        app.buttons["填满屏幕"].tap()
        waitFor("32 by 9 fill covers both viewport dimensions") {
            let value = self.viewportMetrics(viewport)
            let right = (value["displayed_x"] ?? 0) + (value["displayed_width"] ?? 0)
            let bottom = (value["displayed_y"] ?? 0) + (value["displayed_height"] ?? 0)
            return (value["displayed_x"] ?? 1) <= (value["available_x"] ?? 0) + 1
                && (value["displayed_y"] ?? 1) <= (value["available_y"] ?? 0) + 1
                && right >= (value["available_x"] ?? 0) + (value["available_width"] ?? 1) - 1
                && bottom >= (value["available_y"] ?? 0) + (value["available_height"] ?? 1) - 1
        }
        XCTAssertEqual(app.staticTexts["fixture-remote-input"].value as? String, "none")
        attach(app, name: "Production session fixture - ultrawide portrait fill")
    }

    func testObserveOnlyCanZoomWithoutSendingRemoteInput() {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments = ["-ui-viewport-fixture", "-ui-viewport-observe"]
        app.launch()
        defer { app.terminate() }
        let viewport = app.descendants(matching: .any)["remote-viewport"]
        XCTAssertTrue(viewport.waitForExistence(timeout: 15))
        XCTAssertFalse(app.buttons["session-take-over"].isEnabled)
        XCTAssertTrue(app.buttons["session-agent"].isEnabled, "Emergency pause remains available for observe-only devices")
        viewport.pinch(withScale: 2, velocity: 2)
        waitFor("observe-only native zoom") {
            let value = self.viewportMetrics(viewport)
            return (value["zoom"] ?? 0) > (value["fit"] ?? 1) * 1.4
        }
        viewport.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        XCTAssertEqual(app.staticTexts["fixture-remote-input"].value as? String, "none")
        attach(app, name: "Production session fixture - observe-only local zoom")
    }

    private func viewportMetrics(_ viewport: XCUIElement) -> [String: Double] {
        guard let text = viewport.value as? String, let data = text.data(using: .utf8),
              let value = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return [:] }
        return value.compactMapValues { ($0 as? NSNumber)?.doubleValue }
    }

    private func assertCompleteFittedPicture(_ viewport: XCUIElement) {
        let value = viewportMetrics(viewport)
        let x = value["displayed_x"] ?? -1, y = value["displayed_y"] ?? -1
        XCTAssertGreaterThanOrEqual(x, (value["available_x"] ?? 0) - 1, "The source left marker must remain outside the notch")
        XCTAssertGreaterThanOrEqual(y, (value["available_y"] ?? 0) - 1)
        XCTAssertLessThanOrEqual(x + (value["displayed_width"] ?? 0), (value["available_x"] ?? 0) + (value["available_width"] ?? 0) + 1, "The source right marker must be fully visible")
        XCTAssertLessThanOrEqual(y + (value["displayed_height"] ?? 0), (value["available_y"] ?? 0) + (value["available_height"] ?? 0) + 1)
    }

    private func waitFor(_ description: String, condition: @escaping () -> Bool) {
        let predicate = NSPredicate { _, _ in condition() }
        let expectation = XCTNSPredicateExpectation(predicate: predicate, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [expectation], timeout: 15), .completed, description)
    }

    private func assertPointerMapping(app: XCUIApplication, viewport: XCUIElement, start: CGPoint, end: CGPoint? = nil) {
        let value = viewportMetrics(viewport)
        let input = app.staticTexts["fixture-remote-input"]
        let previousSequence = fixtureSequence(input)
        let sourceWidth = 2560.0, sourceHeight = 1080.0
        // Use whole viewport points so HID coordinate quantization does not become
        // a false source-pixel mapping error at portrait fit's small scale.
        func coordinate(_ point: CGPoint) -> CGPoint {
            CGPoint(x: (point.x * viewport.frame.width).rounded() / viewport.frame.width,
                    y: (point.y * viewport.frame.height).rounded() / viewport.frame.height)
        }
        let actualStart = coordinate(start), actualEnd = end.map(coordinate)
        func source(_ point: CGPoint) -> (Double, Double) {
            let u = (Double(point.x) * (value["viewport_width"] ?? 0) - (value["displayed_x"] ?? 0)) / (value["displayed_width"] ?? 1)
            let v = (Double(point.y) * (value["viewport_height"] ?? 0) - (value["displayed_y"] ?? 0)) / (value["displayed_height"] ?? 1)
            return ((u * (sourceWidth - 1)).rounded(), (v * (sourceHeight - 1)).rounded())
        }
        let first = viewport.coordinate(withNormalizedOffset: CGVector(dx: actualStart.x, dy: actualStart.y))
        if let end = actualEnd {
            first.press(forDuration: 0.05, thenDragTo: viewport.coordinate(withNormalizedOffset: CGVector(dx: end.x, dy: end.y)))
        } else { first.tap() }
        waitFor("mapped remote \(end == nil ? "click" : "drag")") {
            guard let text = input.value as? String, let data = text.data(using: .utf8),
                  let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let args = object["args"] as? [String: Any] else { return false }
            return args["action"] as? String == (end == nil ? "click" : "drag")
                && (object["sequence"] as? NSNumber)?.intValue == previousSequence + 1
        }
        let data = (input.value as? String)?.data(using: .utf8) ?? Data()
        guard let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let args = object["args"] as? [String: Any] else { XCTFail("Missing fixture observation"); return }
        XCTAssertEqual((object["sequence"] as? NSNumber)?.intValue, previousSequence + 1, "Each ended gesture must produce exactly one callback")
        let expected = source(actualStart)
        XCTAssertEqual((args["x"] as? NSNumber)?.doubleValue ?? -1, expected.0, accuracy: 2)
        XCTAssertEqual((args["y"] as? NSNumber)?.doubleValue ?? -1, expected.1, accuracy: 2)
        if let end = actualEnd {
            let expectedEnd = source(end)
            XCTAssertEqual((args["to_x"] as? NSNumber)?.doubleValue ?? -1, expectedEnd.0, accuracy: 2)
            XCTAssertEqual((args["to_y"] as? NSNumber)?.doubleValue ?? -1, expectedEnd.1, accuracy: 2)
        }
    }

    private func fixtureSequence(_ input: XCUIElement) -> Int {
        guard let text = input.value as? String, let data = text.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return 0 }
        return (object["sequence"] as? NSNumber)?.intValue ?? 0
    }

    private func attach(_ app: XCUIApplication, name: String) {
        let image = XCTAttachment(screenshot: app.screenshot())
        image.name = name; image.lifetime = .keepAlways; add(image)
    }

    func testLongDummyTicketKeepsPairButtonReachable() {
        let app = XCUIApplication()
        addUIInterruptionMonitor(withDescription: "Local network permission") { alert in
            for label in ["Allow", "允许"] where alert.buttons[label].exists {
                alert.buttons[label].tap()
                return true
            }
            return false
        }
        app.launch()
        XCTAssertTrue(app.buttons["配对一台电脑"].waitForExistence(timeout: 30))
        app.buttons["配对一台电脑"].tap()
        XCTAssertTrue(app.buttons["pair-use-ticket"].waitForExistence(timeout: 10))
        app.buttons["pair-use-ticket"].tap()
        let ticket = app.textViews["pairing-ticket"]
        let submit = app.buttons["pair-submit"]
        XCTAssertTrue(ticket.waitForExistence(timeout: 10))
        // A deliberately invalid, non-secret ticket with the observed real-world length.
        let dummy = "invalid-ticket-" + String(repeating: "x", count: 1111)
        XCTAssertEqual(dummy.count, 1126)
        ticket.tap()
        ticket.typeText(dummy)
        XCTAssertEqual(ticket.value as? String, dummy)
        XCTAssertLessThanOrEqual(ticket.frame.height, 142, "Long text must scroll inside the 140-point editor")
        XCTAssertTrue(submit.exists)
        XCTAssertTrue(submit.isEnabled)
        XCTAssertTrue(app.keyboards.firstMatch.exists, "Verify the action with the editing keyboard open")
        XCTAssertTrue(submit.isHittable, "Pair must remain reachable after entering a long ticket")
        XCTAssertLessThanOrEqual(submit.frame.maxY, app.keyboards.firstMatch.frame.minY, "The pair action must sit above the keyboard")
        let beforeSubmit = XCTAttachment(screenshot: app.screenshot())
        beforeSubmit.name = "Long dummy ticket - keyboard open and pair action visible"
        beforeSubmit.lifetime = .keepAlways
        add(beforeSubmit)
        submit.tap()
        XCTAssertTrue(app.alerts["需要关注"].waitForExistence(timeout: 15))
        app.alerts["需要关注"].buttons["好"].tap()
        XCTAssertEqual(ticket.value as? String, dummy, "An invalid ticket must remain available for correction")
        ticket.tap()
        ticket.typeText(XCUIKeyboardKey.delete.rawValue)
        XCTAssertEqual((ticket.value as? String)?.count, dummy.count - 1, "A rejected ticket must remain editable")
        ticket.typeText("x")
        XCTAssertEqual(ticket.value as? String, dummy, "Editing must update the same ticket after a rejected pairing")
        XCTAssertTrue(submit.isEnabled)
        XCTAssertTrue(submit.isHittable)
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "Long dummy pairing ticket - bounded editor and reachable submit"
        screenshot.lifetime = .keepAlways
        add(screenshot)
        app.terminate()
    }

    func testPairCodeFieldGroupsDigitsAndReportsAnUnknownCode() {
        let app = XCUIApplication()
        addUIInterruptionMonitor(withDescription: "Local network permission") { alert in
            for label in ["Allow", "允许"] where alert.buttons[label].exists {
                alert.buttons[label].tap()
                return true
            }
            return false
        }
        app.launch()
        XCTAssertTrue(app.buttons["配对一台电脑"].waitForExistence(timeout: 30))
        app.buttons["配对一台电脑"].tap()
        let code = app.textFields["pairing-code"]
        let submit = app.buttons["pair-submit"]
        XCTAssertTrue(code.waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["pair-scan"].exists, "Scanning is offered next to typing")
        XCTAssertFalse(submit.isEnabled)
        code.tap()
        code.typeText("1234567")
        XCTAssertEqual(code.value as? String, "1234 567", "Digits are grouped as they are typed")
        XCTAssertFalse(submit.isEnabled, "Seven digits are not a code")
        code.typeText("8")
        XCTAssertEqual(code.value as? String, "1234 5678")
        XCTAssertTrue(submit.isEnabled)
        attach(app, name: "Pair code entry")
        submit.tap()
        // Nobody shows this code: the agent looks for it, then says so.
        XCTAssertTrue(app.alerts["需要关注"].waitForExistence(timeout: 60))
        app.alerts["需要关注"].buttons["好"].tap()
        XCTAssertEqual(code.value as? String, "1234 5678", "A code nobody answered stays for correction")
        app.terminate()
    }

    func testLaunchAndInvalidPairing() {
        let app = XCUIApplication()
        addUIInterruptionMonitor(withDescription: "Local network permission") { alert in
            for label in ["Allow", "允许"] where alert.buttons[label].exists {
                alert.buttons[label].tap()
                return true
            }
            return false
        }
        app.launch()
        XCTAssertTrue(app.buttons["配对一台电脑"].waitForExistence(timeout: 30))
        app.buttons["配对一台电脑"].tap()
        XCTAssertTrue(app.buttons["pair-use-ticket"].waitForExistence(timeout: 10))
        app.buttons["pair-use-ticket"].tap()
        let ticket = app.textViews["pairing-ticket"]
        XCTAssertTrue(ticket.waitForExistence(timeout: 10))
        let submit = app.buttons["pair-submit"]
        XCTAssertFalse(submit.isEnabled)
        ticket.tap()
        ticket.typeText("invalid-ticket")
        XCTAssertTrue(submit.isEnabled)
        submit.tap()
        XCTAssertTrue(app.alerts["需要关注"].waitForExistence(timeout: 15))
        app.alerts["需要关注"].buttons["好"].tap()
        XCTAssertEqual(ticket.value as? String, "invalid-ticket")
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "Phone pairing screen"
        screenshot.lifetime = .keepAlways
        add(screenshot)
        XCUIDevice.shared.press(.home)
        Thread.sleep(forTimeInterval: 2)
        app.activate()
        XCTAssertTrue(ticket.waitForExistence(timeout: 15))
        XCTAssertFalse(app.alerts["需要关注"].waitForExistence(timeout: 5))
        app.buttons["完成"].tap()
        app.terminate()
        app.launch()
        XCTAssertTrue(app.buttons["配对一台电脑"].waitForExistence(timeout: 30))
    }
}

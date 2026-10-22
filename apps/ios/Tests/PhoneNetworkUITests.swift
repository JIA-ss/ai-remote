import XCTest

/// Settings > Network on the phone, against the real embedded core: a new phone is quiet, local
/// network only is one tap, and the pairing sheet says why a pair code cannot work there.
final class PhoneNetworkUITests: XCTestCase {
    func testLocalNetworkOnlyIsOneTapAndPairCodesExplainThemselves() {
        continueAfterFailure = false
        let app = XCUIApplication()
        addUIInterruptionMonitor(withDescription: "Local network permission") { alert in
            for label in ["Allow", "允许"] where alert.buttons[label].exists {
                alert.buttons[label].tap()
                return true
            }
            return false
        }
        app.launch()
        defer { app.terminate() }
        let open = app.buttons["open-network"]
        XCTAssertTrue(open.waitForExistence(timeout: 30))
        open.tap()
        let auto = app.buttons["network-mode-auto"], lan = app.buttons["network-mode-lan"]
        XCTAssertTrue(auto.waitForExistence(timeout: 10))
        if !auto.isSelected { auto.tap() }
        waitFor("automatic") { auto.isSelected && auto.isEnabled }
        // Quiet by default: neither extra is on.
        XCTAssertEqual(app.switches["network-discovery"].value as? String, "0")
        XCTAssertEqual(app.switches["network-upnp"].value as? String, "0")
        attach(app, name: "Network - automatic, quiet defaults")

        lan.tap()
        waitFor("local network only") { lan.isSelected && lan.isEnabled }
        XCTAssertFalse(app.switches["network-upnp"].exists, "No router port mapping without relays")
        XCTAssertTrue(app.switches["network-discovery"].exists)
        attach(app, name: "Network - local network only")
        app.buttons["完成"].tap()
        XCTAssertTrue(app.buttons["network-lan-badge"].waitForExistence(timeout: 10))

        app.buttons["配对电脑"].tap()
        let note = app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "配对码找不到电脑")).firstMatch
        XCTAssertTrue(note.waitForExistence(timeout: 10))
        attach(app, name: "Pairing - local network only without discovery")
        app.buttons["改用完整票据"].firstMatch.tap()
        XCTAssertTrue(app.textViews["pairing-ticket"].waitForExistence(timeout: 10))
        app.buttons["完成"].tap()

        // Back to automatic for the other tests on this simulator.
        XCTAssertTrue(open.waitForExistence(timeout: 10))
        open.tap()
        XCTAssertTrue(auto.waitForExistence(timeout: 10))
        auto.tap()
        waitFor("automatic again") { auto.isSelected && auto.isEnabled }
        app.buttons["完成"].tap()
        XCTAssertFalse(app.buttons["network-lan-badge"].waitForExistence(timeout: 3))
    }

    /// A measurement, skipped unless asked for, while a packet capture runs: switches to local network
    /// only through the sheet, pairs with the ticket in `AR_CAPTURE_TICKET` when there is one, sets the
    /// computer's address to `AR_CAPTURE_ADDRESS` there, opens it and keeps the session for
    /// `AR_CAPTURE_SECONDS`. Pass them to `xcodebuild test` with the `TEST_RUNNER_` prefix.
    func testLocalNetworkOnlySessionForCapture() throws {
        let env = ProcessInfo.processInfo.environment
        guard let address = env["AR_CAPTURE_ADDRESS"], let seconds = env["AR_CAPTURE_SECONDS"].flatMap(Double.init) else {
            throw XCTSkip("a measurement: set TEST_RUNNER_AR_CAPTURE_ADDRESS and TEST_RUNNER_AR_CAPTURE_SECONDS to run it")
        }
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launch()
        defer { app.terminate() }
        let open = app.buttons["open-network"]
        XCTAssertTrue(open.waitForExistence(timeout: 30))
        open.tap()
        let lan = app.buttons["network-mode-lan"]
        XCTAssertTrue(lan.waitForExistence(timeout: 10))
        if !lan.isSelected { lan.tap() }
        waitFor("local network only") { lan.isSelected && lan.isEnabled }
        if let ticket = env["AR_CAPTURE_TICKET"] {
            app.buttons["完成"].tap()
            app.buttons["配对电脑"].tap()
            let useTicket = app.buttons["pair-use-ticket"]
            XCTAssertTrue(useTicket.waitForExistence(timeout: 10))
            useTicket.tap()
            let field = app.textViews["pairing-ticket"]
            XCTAssertTrue(field.waitForExistence(timeout: 10))
            field.tap()
            field.typeText(ticket)
            app.buttons["pair-submit"].tap()
            XCTAssertTrue(app.staticTexts["pair-safety-words"].waitForExistence(timeout: 30), "pairing with the ticket failed")
            attach(app, name: "Paired over the local network only")
            app.buttons["完成"].tap()
            XCTAssertTrue(open.waitForExistence(timeout: 10))
            open.tap()
        }
        let edit = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "network-edit-")).firstMatch
        XCTAssertTrue(edit.waitForExistence(timeout: 10), "pair this simulator's app with a running computer first")
        let host = String(edit.identifier.dropFirst("network-edit-".count))
        edit.tap()
        let field = app.textFields["network-address-field"]
        XCTAssertTrue(field.waitForExistence(timeout: 5))
        field.typeText(address)
        app.buttons["network-address-save"].tap()
        let shown = app.staticTexts["network-address-\(host)"]
        let ip = address.split(separator: ":").first.map(String.init) ?? address
        waitFor("the new address") { (shown.label).contains(ip) && !field.exists }
        attach(app, name: "Network - local network only, address set")
        app.buttons["完成"].tap()
        let card = app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", host)).firstMatch
        XCTAssertTrue(card.waitForExistence(timeout: 10))
        card.tap()
        let started = Date()
        XCTAssertTrue(app.descendants(matching: .any)["remote-viewport"].waitForExistence(timeout: 30), "no picture from \(host) at \(address)")
        attach(app, name: "Session over the local network only")
        RunLoop.current.run(until: started.addingTimeInterval(seconds))
        attach(app, name: "Session after \(Int(seconds)) s")
    }

    private func waitFor(_ what: String, timeout: TimeInterval = 20, _ condition: @escaping () -> Bool) {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return }
            RunLoop.current.run(until: Date().addingTimeInterval(0.2))
        }
        XCTFail("timed out waiting for \(what)")
    }

    private func attach(_ app: XCUIApplication, name: String) {
        let image = XCTAttachment(screenshot: app.screenshot())
        image.name = name; image.lifetime = .keepAlways; add(image)
    }
}

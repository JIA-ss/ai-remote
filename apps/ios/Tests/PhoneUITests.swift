import XCTest
import CoreGraphics

final class PhoneUITests: XCTestCase {
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

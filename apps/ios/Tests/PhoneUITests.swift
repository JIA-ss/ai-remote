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
        // Swipe outside the editor, so an open keyboard can move the Form rather than the text.
        for _ in 0..<2 where !submit.isHittable {
            let start = app.coordinate(withNormalizedOffset: CGVector(dx: 0.03, dy: 0.75))
            let end = app.coordinate(withNormalizedOffset: CGVector(dx: 0.03, dy: 0.35))
            start.press(forDuration: 0.05, thenDragTo: end)
        }
        XCTAssertTrue(submit.isHittable, "Pair must remain reachable after entering a long ticket")
        submit.tap()
        XCTAssertTrue(app.alerts["需要关注"].waitForExistence(timeout: 15))
        app.alerts["需要关注"].buttons["好"].tap()
        XCTAssertEqual(ticket.value as? String, dummy, "An invalid ticket must remain available for correction")
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

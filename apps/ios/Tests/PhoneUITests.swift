import XCTest

final class PhoneUITests: XCTestCase {
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

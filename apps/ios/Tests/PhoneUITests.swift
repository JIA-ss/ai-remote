import XCTest

final class PhoneUITests: XCTestCase {
    func testLaunchAndInvalidPairing() {
        let app = XCUIApplication()
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
        app.terminate()
        app.launch()
        XCTAssertTrue(app.buttons["配对一台电脑"].waitForExistence(timeout: 30))
    }
}

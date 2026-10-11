import XCTest
import UIKit
@testable import AIRemotePhone

/// Native recognizer-state safety checks. These do not simulate a physical hand,
/// transport, or a successful remote input call.
@MainActor final class PhoneViewportGestureTests: XCTestCase {
    private final class BegunPan: RemoteSingleTouchPanRecognizer {
        func beginForAcceptance() { state = .began }
    }

    func testSecondFingerCancelsAnAlreadyBegunRemotePan() {
        let holder = UIView(frame: CGRect(x: 0, y: 0, width: 400, height: 300))
        let pan = BegunPan()
        pan.minimumNumberOfTouches = 1; pan.maximumNumberOfTouches = 1
        holder.addGestureRecognizer(pan)
        pan.activeTouchCountChanged(1)
        pan.beginForAcceptance()
        XCTAssertEqual(pan.state, .began)
        pan.activeTouchCountChanged(2)
        XCTAssertTrue(pan.involvedMultipleTouches)
        XCTAssertEqual(pan.state, .cancelled, "A second finger must revoke a pan that already began")
    }

    func testSecondFingerAlsoRevokesPossibleTapAndPan() {
        let holder = UIView()
        let pan = RemoteSingleTouchPanRecognizer()
        let tap = RemoteSingleTouchTapRecognizer()
        holder.addGestureRecognizer(pan); holder.addGestureRecognizer(tap)
        pan.activeTouchCountChanged(2); tap.activeTouchCountChanged(2)
        XCTAssertEqual(pan.state, .failed)
        XCTAssertEqual(tap.state, .failed)
        XCTAssertTrue(pan.involvedMultipleTouches)
        XCTAssertTrue(tap.involvedMultipleTouches, "A second finger in letterboxing must still cancel a tap")
    }

    func testNativeZoomAndTwoFingerPanCanTakeOverTheSameTouchStream() throws {
        let viewport = NativePhoneViewport(frame: CGRect(x: 0, y: 0, width: 400, height: 300))
        let remote = try XCTUnwrap(viewport.gestureRecognizers?.first(where: { $0 is RemoteSingleTouchPanRecognizer }))
        let pinch = try XCTUnwrap(viewport.pinchGestureRecognizer)
        XCTAssertEqual(viewport.panGestureRecognizer.minimumNumberOfTouches, 2)
        XCTAssertTrue(viewport.gestureRecognizer(remote, shouldRecognizeSimultaneouslyWith: viewport.panGestureRecognizer))
        XCTAssertTrue(viewport.gestureRecognizer(remote, shouldRecognizeSimultaneouslyWith: pinch))
        viewport.allowsRemoteInput = false
        XCTAssertFalse(remote.isEnabled)
        // The end-to-end observe-only UI case verifies that a real native pinch
        // still zooms; this state check verifies the remote recognizer is revoked.
    }

    func testUltrawideFillLimitAndOriginalPixelBounds() throws {
        let viewport = NativePhoneViewport(frame: CGRect(x: 0, y: 0, width: 390, height: 844))
        let context = try XCTUnwrap(CGContext(data: nil, width: 1920, height: 540, bitsPerComponent: 8,
                                             bytesPerRow: 1920 * 4, space: CGColorSpaceCreateDeviceRGB(),
                                             bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue))
        let image = try XCTUnwrap(context.makeImage())
        viewport.show(image, request: PhoneViewportRequest(mode: .fill, revision: 1))
        viewport.layoutIfNeeded()
        XCTAssertEqual(viewport.imageView.bounds.size, CGSize(width: 1920, height: 540))
        XCTAssertGreaterThanOrEqual(viewport.maximumZoomScale, 844.0 / 540.0)
        let displayed = viewport.imageView.convert(viewport.imageView.bounds, to: viewport)
        XCTAssertGreaterThanOrEqual(displayed.width, viewport.bounds.width - 1)
        XCTAssertGreaterThanOrEqual(displayed.height, viewport.bounds.height - 1)
    }
}

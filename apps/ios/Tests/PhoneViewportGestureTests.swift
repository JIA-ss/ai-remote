import XCTest
import UIKit
@testable import AIRemotePhone

/// Native cancellation decisions and completion-policy checks. These do not simulate a physical hand,
/// transport, or a successful remote input call.
@MainActor final class PhoneViewportGestureTests: XCTestCase {
    private func viewport() throws -> NativePhoneViewport {
        let view = NativePhoneViewport(frame: CGRect(x: 0, y: 0, width: 400, height: 300))
        let context = try XCTUnwrap(CGContext(data: nil, width: 320, height: 180, bitsPerComponent: 8,
                                             bytesPerRow: 320 * 4, space: CGColorSpaceCreateDeviceRGB(),
                                             bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue))
        view.show(try XCTUnwrap(context.makeImage()), request: PhoneViewportRequest())
        view.layoutIfNeeded()
        view.allowsRemoteInput = true
        return view
    }

    func testSecondFingerCancellationDecisionRejectsRemoteCompletion() throws {
        let view = try viewport()
        let pan = try XCTUnwrap(view.gestureRecognizers?.first(where: { $0 is RemoteSingleTouchPanRecognizer }) as? RemoteSingleTouchPanRecognizer)
        var callbacks = 0
        view.onRemoteGesture = { _ in callbacks += 1 }
        let gesture = PhonePointerGesture(start: CGPoint(x: 0.2, y: 0.5), end: CGPoint(x: 0.8, y: 0.5), isDrag: true)
        XCTAssertFalse(pan.activeTouchCountChanged(1))
        XCTAssertTrue(view.deliver(gesture, multipleTouches: pan.involvedMultipleTouches))
        XCTAssertEqual(callbacks, 1)
        XCTAssertTrue(pan.activeTouchCountChanged(2))
        XCTAssertTrue(pan.involvedMultipleTouches)
        XCTAssertFalse(view.deliver(gesture, multipleTouches: pan.involvedMultipleTouches))
        XCTAssertEqual(callbacks, 1, "A completion involving the second finger must not reach the callback")
    }

    func testSecondFingerAlsoRevokesTapAndPanCompletion() throws {
        let view = try viewport()
        let pan = try XCTUnwrap(view.gestureRecognizers?.first(where: { $0 is RemoteSingleTouchPanRecognizer }) as? RemoteSingleTouchPanRecognizer)
        let tap = try XCTUnwrap(view.gestureRecognizers?.first(where: { $0 is RemoteSingleTouchTapRecognizer }) as? RemoteSingleTouchTapRecognizer)
        var callbacks = 0
        view.onRemoteGesture = { _ in callbacks += 1 }
        XCTAssertTrue(pan.activeTouchCountChanged(2)); XCTAssertTrue(tap.activeTouchCountChanged(2))
        XCTAssertTrue(pan.involvedMultipleTouches)
        XCTAssertTrue(tap.involvedMultipleTouches, "A second finger in letterboxing must still cancel a tap")
        let point = PhonePointerGesture(start: CGPoint(x: 0.5, y: 0.5), end: CGPoint(x: 0.5, y: 0.5), isDrag: false)
        XCTAssertFalse(view.deliver(point, multipleTouches: pan.involvedMultipleTouches))
        XCTAssertFalse(view.deliver(point, multipleTouches: tap.involvedMultipleTouches))
        XCTAssertEqual(callbacks, 0)
    }

    func testNativeZoomAndTwoFingerPanCanTakeOverTheSameTouchStream() throws {
        let viewport = try self.viewport()
        let remote = try XCTUnwrap(viewport.gestureRecognizers?.first(where: { $0 is RemoteSingleTouchPanRecognizer }))
        let pinch = try XCTUnwrap(viewport.pinchGestureRecognizer)
        XCTAssertEqual(viewport.panGestureRecognizer.minimumNumberOfTouches, 2)
        XCTAssertTrue(viewport.gestureRecognizer(remote, shouldRecognizeSimultaneouslyWith: viewport.panGestureRecognizer))
        XCTAssertTrue(viewport.gestureRecognizer(remote, shouldRecognizeSimultaneouslyWith: pinch))
        XCTAssertTrue(viewport.gestureRecognizer(viewport.panGestureRecognizer, shouldRecognizeSimultaneouslyWith: pinch))
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

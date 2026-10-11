import Foundation
import CoreGraphics

@main
struct PhoneViewportGeometrySmoke {
    static func main() {
        portraitAndLandscapeFit()
        fractionalPointsAndUpscaling()
        imageBoundsAndLetterboxing()
        zoomAndPanMapToHostPoints()
        rotationAndCenteredOffsets()
        invalidGeometry()
        print("PASS: viewport fit, fractional points, letterbox rejection, zoom/pan host mapping, rotation bounds and invalid input")
    }

    private static func near(_ actual: CGFloat, _ expected: CGFloat, _ context: String) {
        precondition(abs(actual - expected) <= max(1, abs(expected)) * 1e-10,
                     "\(context): expected \(expected), got \(actual)")
    }

    private static func near(_ actual: CGPoint, _ expected: CGPoint, _ context: String) {
        near(actual.x, expected.x, "\(context).x")
        near(actual.y, expected.y, "\(context).y")
    }

    private static func portraitAndLandscapeFit() {
        let image = CGSize(width: 1920, height: 1080)
        let portrait = CGSize(width: 390, height: 844)
        let landscape = CGSize(width: 844, height: 390)
        let portraitFit = PhoneViewportGeometry.fitScale(image: image, viewport: portrait)!
        let landscapeFit = PhoneViewportGeometry.fitScale(image: image, viewport: landscape)!
        near(portraitFit, 0.203125, "portrait fit fills the width")
        near(landscapeFit, 13.0 / 36.0, "landscape fit fills the height")
        near(1920 * portraitFit, 390, "portrait visible width")
        near(1080 * portraitFit, 219.375, "portrait visible height")
        near(1920 * landscapeFit, 693.3333333333334, "landscape visible width")
        near(1080 * landscapeFit, 390, "landscape visible height")
        precondition(landscapeFit > portraitFit, "Rotating this desktop to landscape must enlarge it")
        for viewport in [portrait, landscape] {
            let fit = PhoneViewportGeometry.fitScale(image: image, viewport: viewport)!
            precondition(image.width * fit <= viewport.width + 1e-9 && image.height * fit <= viewport.height + 1e-9)
            precondition(image.width * (fit * 1.01) > viewport.width || image.height * (fit * 1.01) > viewport.height,
                         "A larger scale must clip at least one edge; fit must use the available viewport")
        }
    }

    private static func fractionalPointsAndUpscaling() {
        let image = CGSize(width: 1919.5, height: 1079.75)
        let viewport = CGSize(width: 479.875, height: 300.125)
        let fit = PhoneViewportGeometry.fitScale(image: image, viewport: viewport)!
        near(fit, 0.25, "fractional image and viewport sizes")
        let inset = PhoneViewportGeometry.centeringOffset(contentSize: CGSize(width: 479.875, height: 269.9375),
                                                         viewport: viewport)!
        near(inset, CGPoint(x: 0, y: 15.09375), "fractional centering must not round to integer pixels")
        near(PhoneViewportGeometry.normalizedPoint(CGPoint(x: 479.875, y: 809.8125), imageSize: image)!,
             CGPoint(x: 0.25, y: 0.75), "fractional image coordinates")
        near(PhoneViewportGeometry.fitScale(image: CGSize(width: 300, height: 200),
                                           viewport: CGSize(width: 1200, height: 800))!, 4, "small image can fill the viewport")
    }

    private static func imageBoundsAndLetterboxing() {
        let image = CGSize(width: 1920, height: 1080)
        near(PhoneViewportGeometry.normalizedPoint(.zero, imageSize: image)!, .zero, "first image edge")
        near(PhoneViewportGeometry.normalizedPoint(CGPoint(x: 1920, y: 1080), imageSize: image)!,
             CGPoint(x: 1, y: 1), "inclusive final image edge")
        for outside in [CGPoint(x: -0.001, y: 540), CGPoint(x: 1920.001, y: 540),
                        CGPoint(x: 960, y: -0.001), CGPoint(x: 960, y: 1080.001)] {
            precondition(PhoneViewportGeometry.normalizedPoint(outside, imageSize: image) == nil,
                         "Outside input must be rejected, never converted into an edge click")
        }
        // Independent affine fixture: the portrait desktop starts at y=312.3125 points.
        let toViewport = CGAffineTransform(a: 0.203125, b: 0, c: 0, d: 0.203125, tx: 0, ty: 312.3125)
        let letterbox = CGPoint(x: 195, y: 200).applying(toViewport.inverted())
        precondition(PhoneViewportGeometry.normalizedPoint(letterbox, imageSize: image) == nil)
        let pictureCenter = CGPoint(x: 195, y: 422).applying(toViewport.inverted())
        near(PhoneViewportGeometry.normalizedPoint(pictureCenter, imageSize: image)!,
             CGPoint(x: 0.5, y: 0.5), "viewport center maps to picture center")
    }

    private static func zoomAndPanMapToHostPoints() {
        let image = CGSize(width: 1600, height: 900)
        let screen = ScreenGeometry(.object(["x": .number(-1600), "y": .number(100),
                                             "width": .number(1601), "height": .number(901)]))!
        // Host points differ from encoded pixels. Test the actual ScreenGeometry endpoint mapping.
        let cases: [(CGPoint, CGPoint, CGPoint, CGPoint)] = [
            (CGPoint(x: 640, y: 360), CGPoint(x: 160, y: 90), CGPoint(x: 400, y: 225), CGPoint(x: -1200, y: 325)),
            (CGPoint(x: 1280, y: 720), CGPoint(x: 160, y: 90), CGPoint(x: 720, y: 405), CGPoint(x: -880, y: 505)),
        ]
        for (offset, touch, imagePoint, expectedHost) in cases {
            let toViewport = CGAffineTransform(a: 2, b: 0, c: 0, d: 2, tx: -offset.x, ty: -offset.y)
            near(imagePoint.applying(toViewport), touch, "forward pixel to zoomed/panned viewport")
            let converted = touch.applying(toViewport.inverted())
            near(converted, imagePoint, "UIKit-style inverse restores original pixels")
            let normalized = PhoneViewportGeometry.normalizedPoint(converted, imageSize: image)!
            let host = screen.point(u: Double(normalized.x), v: Double(normalized.y))!
            near(CGPoint(x: host.0, y: host.1), expectedHost, "zoom/pan remote input")
        }
        let upperLeft = screen.point(u: 0, v: 0)!, lowerRight = screen.point(u: 1, v: 1)!
        near(CGPoint(x: upperLeft.0, y: upperLeft.1), CGPoint(x: -1600, y: 100), "negative display origin")
        near(CGPoint(x: lowerRight.0, y: lowerRight.1), CGPoint(x: 0, y: 1000), "last host point, not one past the screen")
        // A 2x Retina capture uses 2880x1800 pixels for a 1440x900 host-point display.
        let retinaScreen = ScreenGeometry(.object(["x": .number(-1440), "y": .number(-900),
                                                   "width": .number(1440), "height": .number(900)]))!
        let normalized = PhoneViewportGeometry.normalizedPoint(CGPoint(x: 720, y: 1350),
                                                               imageSize: CGSize(width: 2880, height: 1800))!
        let retinaHost = retinaScreen.point(u: Double(normalized.x), v: Double(normalized.y))!
        near(CGPoint(x: retinaHost.0, y: retinaHost.1), CGPoint(x: -1080, y: -226), "capture pixels are not host points")
    }

    private static func rotationAndCenteredOffsets() {
        let content = CGSize(width: 1280, height: 720), rotatedViewport = CGSize(width: 852, height: 393)
        let center = PhoneViewportGeometry.centeringOffset(contentSize: content, viewport: rotatedViewport)!
        near(center, .zero, "large content needs no centering inset")
        near(PhoneViewportGeometry.clampOffset(CGPoint(x: 500, y: 400), contentSize: content,
                                              viewport: rotatedViewport, centering: center)!,
             CGPoint(x: 428, y: 327), "rotation removes newly out-of-range offsets")
        let smallContent = CGSize(width: 390, height: 219.375), portrait = CGSize(width: 390, height: 844)
        let centered = PhoneViewportGeometry.centeringOffset(contentSize: smallContent, viewport: portrait)!
        near(centered, CGPoint(x: 0, y: 312.3125), "portrait symmetric letterbox")
        for offset in [CGPoint(x: 20, y: 0), CGPoint(x: -30, y: -800), CGPoint(x: 900, y: 900)] {
            near(PhoneViewportGeometry.clampOffset(offset, contentSize: smallContent, viewport: portrait,
                                                  centering: centered)!, CGPoint(x: 0, y: -312.3125),
                 "small content has one centered scroll position")
        }
        let mixed = CGSize(width: 1200, height: 300), viewport = CGSize(width: 800, height: 600)
        near(PhoneViewportGeometry.clampOffset(CGPoint(x: 999, y: 777), contentSize: mixed, viewport: viewport,
                                              centering: CGPoint(x: 0, y: 150))!, CGPoint(x: 400, y: -150),
             "horizontal pan and vertical centering can coexist")
        let zoomed = CGSize(width: 3200, height: 1800), view = CGSize(width: 800, height: 450)
        near(PhoneViewportGeometry.clampOffset(CGPoint(x: -100, y: 1800), contentSize: zoomed, viewport: view,
                                              centering: .zero)!, CGPoint(x: 0, y: 1350), "zoomed scroll bounds")
    }

    private static func invalidGeometry() {
        let valid = CGSize(width: 800, height: 450)
        for size in [CGSize.zero, CGSize(width: -1, height: 10), CGSize(width: 10, height: 0),
                     CGSize(width: CGFloat.nan, height: 10), CGSize(width: 10, height: CGFloat.infinity)] {
            precondition(PhoneViewportGeometry.fitScale(image: size, viewport: valid) == nil)
            precondition(PhoneViewportGeometry.fitScale(image: valid, viewport: size) == nil)
            precondition(PhoneViewportGeometry.normalizedPoint(.zero, imageSize: size) == nil)
            precondition(PhoneViewportGeometry.centeringOffset(contentSize: size, viewport: valid) == nil)
            precondition(PhoneViewportGeometry.clampOffset(.zero, contentSize: valid, viewport: size, centering: .zero) == nil)
        }
        for point in [CGPoint(x: CGFloat.nan, y: 0), CGPoint(x: 0, y: CGFloat.infinity)] {
            precondition(PhoneViewportGeometry.normalizedPoint(point, imageSize: valid) == nil)
            precondition(PhoneViewportGeometry.clampOffset(point, contentSize: valid, viewport: valid, centering: .zero) == nil)
            precondition(PhoneViewportGeometry.clampOffset(.zero, contentSize: valid, viewport: valid, centering: point) == nil)
        }
        precondition(PhoneViewportGeometry.clampOffset(.zero, contentSize: valid, viewport: valid,
                                                      centering: CGPoint(x: -1, y: 0)) == nil)
        precondition(PhoneViewportGeometry.fitScale(image: CGSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude),
                                                   viewport: CGSize(width: CGFloat.leastNonzeroMagnitude, height: CGFloat.leastNonzeroMagnitude)) == nil,
                     "Underflow to a zero zoom scale must be refused")
        precondition(PhoneViewportGeometry.fitScale(image: CGSize(width: CGFloat.leastNonzeroMagnitude, height: CGFloat.leastNonzeroMagnitude),
                                                   viewport: CGSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)) == nil,
                     "Overflow to an infinite zoom scale must be refused")
        precondition(PhoneViewportGeometry.clampOffset(.zero,
                                                      contentSize: CGSize(width: CGFloat.greatestFiniteMagnitude, height: 1),
                                                      viewport: CGSize(width: 1, height: 1),
                                                      centering: CGPoint(x: CGFloat.greatestFiniteMagnitude, y: 0)) == nil)
    }
}

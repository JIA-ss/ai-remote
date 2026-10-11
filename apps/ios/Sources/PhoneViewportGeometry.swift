import Foundation

/// Geometry shared by the native viewport and its input mapping, without UIKit dependencies.
public enum PhoneViewportGeometry {
    /// Viewport points per image pixel. Fitting may enlarge a small image.
    public static func fitScale(image: CGSize, viewport: CGSize) -> CGFloat? {
        guard valid(image), valid(viewport) else { return nil }
        let scale = min(viewport.width / image.width, viewport.height / image.height)
        return scale.isFinite && scale > 0 ? scale : nil
    }

    /// `point` is in the image view's original pixel bounds, after UIKit reverses zoom and pan.
    /// Reject letterboxing and out-of-image input instead of clamping it to a remote edge.
    public static func normalizedPoint(_ point: CGPoint, imageSize: CGSize) -> CGPoint? {
        guard valid(imageSize), point.x.isFinite, point.y.isFinite,
              point.x >= 0, point.y >= 0,
              point.x <= imageSize.width, point.y <= imageSize.height else { return nil }
        return CGPoint(x: point.x / imageSize.width, y: point.y / imageSize.height)
    }

    /// Symmetric inset on each side of scaled content: x is left/right, y is top/bottom.
    public static func centeringOffset(contentSize: CGSize, viewport: CGSize) -> CGPoint? {
        guard valid(contentSize), valid(viewport) else { return nil }
        return CGPoint(x: max(0, (viewport.width - contentSize.width) / 2),
                       y: max(0, (viewport.height - contentSize.height) / 2))
    }

    /// UIScrollView offsets include the negative inset when content is smaller than the viewport.
    /// Use the current scaled content size and symmetric inset after zooming or rotating.
    public static func clampOffset(_ offset: CGPoint, contentSize: CGSize, viewport: CGSize,
                                   centering: CGPoint) -> CGPoint? {
        guard valid(contentSize), valid(viewport), offset.x.isFinite, offset.y.isFinite,
              centering.x.isFinite, centering.y.isFinite,
              centering.x >= 0, centering.y >= 0 else { return nil }
        let minX = -centering.x, minY = -centering.y
        let maxX = max(minX, contentSize.width + centering.x - viewport.width)
        let maxY = max(minY, contentSize.height + centering.y - viewport.height)
        guard maxX.isFinite, maxY.isFinite else { return nil }
        return CGPoint(x: min(max(offset.x, minX), maxX), y: min(max(offset.y, minY), maxY))
    }

    private static func valid(_ size: CGSize) -> Bool {
        size.width.isFinite && size.height.isFinite && size.width > 0 && size.height > 0
    }
}

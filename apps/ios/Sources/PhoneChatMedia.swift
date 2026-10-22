import Foundation
import CoreGraphics
import ImageIO
import UniformTypeIdentifiers

// Images for the chat: a photo from the library or the camera made ready to send (the host takes
// PNG, JPEG, GIF and WebP up to 3.75 MiB each; the long edge is cut to 2048 like the desktop does),
// and a decoded preview image. ImageIO only, so it builds and runs on macOS as well.

/// The bytes of a picked or taken photo as something the host takes: kept as they are when they
/// already are a PNG, JPEG, GIF or WebP within the size and edge limits, otherwise scaled to a long
/// edge of `phoneImageEdge` and encoded as JPEG (a HEIC photo from the camera always is). nil when the
/// bytes are not an image.
public func phonePrepareImage(_ data: Data, name: String) -> PhoneImage? {
    guard let source = CGImageSourceCreateWithData(data as CFData, nil), CGImageSourceGetCount(source) > 0 else { return nil }
    let type = (CGImageSourceGetType(source) as String?).flatMap { UTType($0) }
    let mime = type?.preferredMIMEType ?? ""
    let props = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any] ?? [:]
    let width = props[kCGImagePropertyPixelWidth] as? Int ?? 0, height = props[kCGImagePropertyPixelHeight] as? Int ?? 0
    let orientation = props[kCGImagePropertyOrientation] as? Int ?? 1
    let base = (name as NSString).deletingPathExtension.isEmpty ? "image" : (name as NSString).deletingPathExtension
    if phoneImageTypes.contains(mime), data.count <= phoneMaxImageBytes, !phoneFitWithin(width: width, height: height).scaled, orientation == 1 {
        let ext = type?.preferredFilenameExtension ?? "png"
        return PhoneImage(name: base + "." + ext, mime: mime, data: data)
    }
    // Scaled (and turned upright) once, then encoded smaller until it fits.
    let options: [CFString: Any] = [kCGImageSourceCreateThumbnailFromImageAlways: true, kCGImageSourceCreateThumbnailWithTransform: true,
                                    kCGImageSourceThumbnailMaxPixelSize: min(phoneImageEdge, max(width, height, 1))]
    guard let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else { return nil }
    for quality in [0.85, 0.7, 0.5, 0.35] {
        if let jpeg = phoneEncode(image, type: .jpeg, quality: quality), jpeg.count <= phoneMaxImageBytes {
            return PhoneImage(name: base + ".jpg", mime: "image/jpeg", data: jpeg)
        }
    }
    return nil
}

/// An image encoded as `type`.
public func phoneEncode(_ image: CGImage, type: UTType, quality: Double = 0.85) -> Data? {
    let out = NSMutableData()
    guard let dest = CGImageDestinationCreateWithData(out, type.identifier as CFString, 1, nil) else { return nil }
    CGImageDestinationAddImage(dest, image, [kCGImageDestinationLossyCompressionQuality: quality] as CFDictionary)
    return CGImageDestinationFinalize(dest) ? out as Data : nil
}

/// A decoded image (the first frame), for thumbnails and previews; nil for bytes ImageIO cannot read
/// (an SVG is drawn by the preview's web view instead).
public func phoneDecodeImage(_ data: Data, maxPixels: Int? = nil) -> CGImage? {
    guard let source = CGImageSourceCreateWithData(data as CFData, nil), CGImageSourceGetCount(source) > 0 else { return nil }
    if let maxPixels {
        let options: [CFString: Any] = [kCGImageSourceCreateThumbnailFromImageAlways: true, kCGImageSourceCreateThumbnailWithTransform: true,
                                        kCGImageSourceThumbnailMaxPixelSize: maxPixels]
        return CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
    }
    return CGImageSourceCreateImageAtIndex(source, 0, nil)
}

/// A small chart-like PNG, for the simulator fixture and tests (no photo library there).
public func phoneFixturePNG(width: Int, height: Int) -> Data {
    let space = CGColorSpaceCreateDeviceRGB()
    guard let ctx = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0, space: space,
                              bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return Data() }
    ctx.setFillColor(CGColor(red: 0.96, green: 0.97, blue: 0.99, alpha: 1))
    ctx.fill(CGRect(x: 0, y: 0, width: width, height: height))
    let colors: [CGColor] = [CGColor(red: 0.15, green: 0.39, blue: 0.92, alpha: 1), CGColor(red: 0.09, green: 0.64, blue: 0.29, alpha: 1),
                             CGColor(red: 0.92, green: 0.35, blue: 0.05, alpha: 1)]
    let bars = [0.45, 0.8, 0.6, 0.95, 0.35]
    let w = Double(width) / Double(bars.count * 2 + 1)
    for (i, h) in bars.enumerated() {
        ctx.setFillColor(colors[i % colors.count])
        ctx.fill(CGRect(x: w * Double(i * 2 + 1), y: 12, width: w, height: (Double(height) - 24) * h))
    }
    guard let image = ctx.makeImage() else { return Data() }
    return phoneEncode(image, type: .png) ?? Data()
}

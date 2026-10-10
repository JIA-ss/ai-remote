// Rasterize the existing vector mark onto an opaque iOS tile. Apple applies the corner mask.
// Run from the repository root: swift tools/brand/build_ios_icon.swift
import AppKit
import Foundation

let svg = try String(contentsOfFile: "assets/brand/app-icon.svg", encoding: .utf8)
let pixels = CGContext(data: nil, width: 1024, height: 1024, bitsPerComponent: 8,
    bytesPerRow: 4096, space: CGColorSpaceCreateDeviceRGB(),
    bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)!
let context = NSGraphicsContext(cgContext: pixels, flipped: false)
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = context
func color(_ hex: String) -> NSColor {
    let value = Int(hex.dropFirst(), radix: 16)!
    return NSColor(red: CGFloat((value >> 16) & 255) / 255, green: CGFloat((value >> 8) & 255) / 255,
        blue: CGFloat(value & 255) / 255, alpha: 1)
}
NSGradient(starting: color("#11151B"), ending: color("#252C38"))!
    .draw(in: NSRect(x: 0, y: 0, width: 1024, height: 1024), angle: 90)
let pattern = try NSRegularExpression(pattern: #"<polygon points="([^"]+)" fill="([^"]+)""#)
for match in pattern.matches(in: svg, range: NSRange(svg.startIndex..., in: svg)) {
    let points = svg[Range(match.range(at: 1), in: svg)!].split(separator: " ").map {
        $0.split(separator: ",").map { Double($0)! }
    }
    let path = NSBezierPath()
    for (index, point) in points.enumerated() {
        let p = NSPoint(x: point[0], y: 1024 - point[1])
        if index == 0 { path.move(to: p) } else { path.line(to: p) }
    }
    path.close(); path.lineWidth = 9.1; path.lineJoinStyle = .round
    color(String(svg[Range(match.range(at: 2), in: svg)!])).set()
    path.fill(); path.stroke()
}
NSGraphicsContext.restoreGraphicsState()
let path = "apps/ios/App/Assets.xcassets/AppIcon.appiconset/AppIcon.png"
let bitmap = NSBitmapImageRep(cgImage: pixels.makeImage()!)
try bitmap.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: path))

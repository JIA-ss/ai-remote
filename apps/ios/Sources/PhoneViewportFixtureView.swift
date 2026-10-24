#if DEBUG && os(iOS)
import SwiftUI
import CoreGraphics

/// Simulator UI acceptance only. Reuses the production session and UIKit viewport,
/// while replacing transport with an explicit static picture and input observer.
public struct PhoneViewportFixtureView: View {
    @PhoneViewState<PhoneStore> private var store: PhoneStore
    @PhoneViewState<String> private var lastInput = "none"
    @PhoneViewState<Int> private var inputCount = 0
    @PhoneViewState private var resizeFixtureNarrow = false
    @PhoneViewState<CGFloat?> private var lastWindowWidth = nil

    public init() {
        let model = PhoneStore()
        model.selected = PhoneHost(.object(["name": .string("UI 测试静态画面"), "level_at_pairing": .string("control")]))
        let ultrawide = ProcessInfo.processInfo.arguments.contains("-ui-viewport-ultrawide")
        model.image = Self.picture(height: ultrawide ? 540 : 810)
        model.screen = ScreenGeometry(.object(["x": .number(0), "y": .number(0), "width": .number(ultrawide ? 3840 : 2560), "height": .number(1080)]))
        model.connected = true
        let observe = ProcessInfo.processInfo.arguments.contains("-ui-viewport-observe")
        model.level = observe ? "observe" : "control"
        model.driver = observe ? "agent" : "person"
        model.ownName = "ui-fixture"
        model.driverDevice = "ui-fixture"
        if ProcessInfo.processInfo.arguments.contains("-ui-viewport-chat") {
            model.chat = PhoneChatFixtureView.makeStore(host: model.selected!.name)
            model.showChat = true
        }
        _store = State(initialValue: model)
    }

    public var body: some View {
        GeometryReader { area in
            let resized = ProcessInfo.processInfo.arguments.contains("-ui-viewport-resize") && resizeFixtureNarrow
            PhoneRootView(store: store, fixtureInputObserver: { tool, args in
                inputCount += 1
                lastInput = JSONValue.object(["sequence": .number(Double(inputCount)), "tool": .string(tool), "args": .object(args)]).text
            })
            // Exercise the production layout with a half-width container. This is not
            // a claim that the simulator entered the system's Split View mode.
            .frame(width: resized ? 683 : nil)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .onChange(of: area.size.width, initial: true) { _, width in
                // Follow the actual window-width transition. Keyboard height cannot flip
                // this state, and device-orientation notifications are not required.
                if let previous = lastWindowWidth, previous != width { resizeFixtureNarrow = width < previous }
                else if lastWindowWidth == nil { resizeFixtureNarrow = width < area.size.height }
                lastWindowWidth = width
            }
        }
        .overlay(alignment: .bottomLeading) {
            Text("UI 测试 · 静态画面")
                .font(.caption2).foregroundStyle(.white).padding(4).background(.black.opacity(0.7))
                .accessibilityIdentifier("fixture-remote-input").accessibilityValue(lastInput)
                .padding(.leading, 8).padding(.bottom, 76).allowsHitTesting(false)
        }
    }

    private static func picture(height: Int) -> CGImage {
        let width = 1920
        let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8,
                                bytesPerRow: width * 4, space: CGColorSpaceCreateDeviceRGB(),
                                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        context.setFillColor(CGColor(red: 0.06, green: 0.1, blue: 0.17, alpha: 1))
        context.fill(CGRect(x: 0, y: 0, width: width, height: height))
        context.setStrokeColor(CGColor(red: 0.25, green: 0.4, blue: 0.6, alpha: 1))
        context.setLineWidth(2)
        for x in stride(from: 0, through: width, by: 120) {
            context.move(to: CGPoint(x: x, y: 0)); context.addLine(to: CGPoint(x: x, y: height))
        }
        for y in stride(from: 0, through: height, by: 90) {
            context.move(to: CGPoint(x: 0, y: y)); context.addLine(to: CGPoint(x: width, y: y))
        }
        context.strokePath()
        for (color, rect) in [
            (CGColor(red: 0.15, green: 0.75, blue: 0.4, alpha: 1), CGRect(x: 0, y: 0, width: 200, height: 200)),
            (CGColor(red: 0.9, green: 0.3, blue: 0.18, alpha: 1), CGRect(x: 1720, y: height - 200, width: 200, height: 200)),
            (CGColor(red: 0.2, green: 0.65, blue: 0.95, alpha: 1), CGRect(x: 900, y: height / 2 - 60, width: 120, height: 120))
        ] { context.setFillColor(color); context.fill(rect) }
        return context.makeImage()!
    }
}
#endif

import Foundation

@main struct PhoneAdaptiveLayoutSmoke {
    static func main() {
        // Window sizes in points for the priority 12.9-inch iPad, its multitasking
        // windows, and a phone. Device family alone must never force wide chrome.
        let cases: [(String, CGFloat, Bool, Bool, Bool, Int)] = [
            ("12.9-inch landscape", 1366, true, true, true, 2),
            ("12.9-inch portrait", 1024, true, true, true, 2),
            ("two-thirds window", 910, true, true, true, 2),
            ("half-screen window", 683, true, false, false, 1),
            ("one-third window", 438, false, false, false, 1),
            ("narrow Stage Manager", 744, true, false, false, 1),
            ("standalone sidebar threshold", 800, true, true, false, 2),
            ("below sidebar threshold", 799, true, false, false, 2),
            ("companion threshold", 900, true, true, true, 2),
            ("below companion threshold", 899, true, true, false, 2),
            ("phone landscape", 932, false, false, false, 1),
            ("embedded chat panel", 440, true, false, false, 1),
        ]
        for (name, width, regular, sidebar, companion, columns) in cases {
            let layout = PhoneAdaptiveLayout(width: width, regular: regular)
            precondition(layout.chatSidebar == sidebar, "\(name): session sidebar")
            precondition(layout.companionChat == companion, "\(name): companion chat")
            precondition(layout.computerColumns == columns, "\(name): computer columns")
            if companion {
                precondition(layout.chatPanelWidth >= 360 && layout.chatPanelWidth <= 440, "\(name): usable composer")
                precondition(width - layout.chatPanelWidth - 1 >= 520, "\(name): remote controls fit")
                precondition(!PhoneAdaptiveLayout(width: layout.chatPanelWidth, regular: true).chatSidebar,
                             "\(name): no nested sidebar in the companion")
            }
        }
        print("Adaptive layout: \(cases.count) iPad, multitasking and phone cases passed")
    }
}

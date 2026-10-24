import Foundation

/// Use the window's available points, rather than a device model or screen bounds:
/// a 12.9-inch iPad can also be a narrow Split View or Stage Manager window.
struct PhoneAdaptiveLayout: Equatable {
    let width: CGFloat
    let regular: Bool

    static let transcriptWidth: CGFloat = 820
    static let computersWidth: CGFloat = 1100

    var chatSidebar: Bool { regular && width >= 800 }
    var companionChat: Bool { regular && width >= 900 }
    var chatPanelWidth: CGFloat { min(440, max(360, width * 0.32)) }
    var computerColumns: Int { regular && width >= 760 ? 2 : 1 }
}

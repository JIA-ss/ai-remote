// A disposable native window for the macOS input E2E. Never type into the user's applications.
import Cocoa

final class Fixture: NSObject, NSApplicationDelegate, NSTextFieldDelegate {
    var window: NSWindow!
    let field = NSTextField(string: "")
    let button = NSButton(title: "Click test", target: nil, action: nil)
    var clicks = 0
    let output = CommandLine.arguments[1]

    func applicationDidFinishLaunching(_ notification: Notification) {
        let menu = NSMenu()
        let edit = NSMenuItem(title: "Edit", action: nil, keyEquivalent: "")
        let editMenu = NSMenu(title: "Edit")
        editMenu.addItem(NSMenuItem(title: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a"))
        edit.submenu = editMenu
        menu.addItem(edit)
        NSApp.mainMenu = menu
        window = NSWindow(contentRect: NSRect(x: 100, y: 100, width: 400, height: 210),
                          styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.title = "ai-remote macOS E2E"
        button.target = self
        button.action = #selector(clicked)
        button.frame = NSRect(x: 40, y: 120, width: 180, height: 40)
        field.frame = NSRect(x: 40, y: 60, width: 320, height: 30)
        field.placeholderString = "Input test"
        field.delegate = self
        window.contentView!.addSubview(button)
        window.contentView!.addSubview(field)
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        _ = Timer.scheduledTimer(withTimeInterval: 0.2, repeats: true) { _ in self.save() }
        save()
    }

    @objc func clicked() { clicks += 1; save() }
    func controlTextDidChange(_ notification: Notification) { save() }
    func save() {
        guard window != nil else { return }
        let top = NSScreen.screens.first!.frame.maxY
        func center(_ view: NSView) -> [String: Double] {
            let r = window.convertToScreen(view.frame)
            return ["x": r.midX, "y": top - r.midY]
        }
        let payload: [String: Any] = ["clicks": clicks, "text": field.stringValue,
                                      "button": center(button), "field": center(field),
                                      "editing": field.currentEditor() != nil,
                                      "frontmost": window.isKeyWindow && NSApp.isActive]
        if let bytes = try? JSONSerialization.data(withJSONObject: payload) {
            try? bytes.write(to: URL(fileURLWithPath: output), options: .atomic)
        }
    }
}

let app = NSApplication.shared
let fixture = Fixture()
app.setActivationPolicy(.regular)
app.delegate = fixture
app.run()

// Disposable AX fixture. State is independent of AX readback.
import Cocoa
final class Fixture: NSObject, NSApplicationDelegate {
    var window: NSWindow!
    let field = NSTextField(string: "")
    let secure = NSSecureTextField(string: "fixture-secret-do-not-export")
    let button = NSButton(title: "Apply", target: nil, action: nil)
    let disabled = NSButton(title: "Disabled", target: nil, action: nil)
    let check = NSButton(checkboxWithTitle: "Enabled option", target: nil, action: nil)
    var clicks = 0
    let output = CommandLine.arguments[1]
    func applicationDidFinishLaunching(_ n: Notification) {
        window = NSWindow(contentRect: NSRect(x: 120,y: 120,width: 460,height: 300), styleMask: [.titled,.closable],backing: .buffered,defer: false)
        window.title = "ai-remote structured UI fixture"
        field.frame = NSRect(x: 30,y: 220,width: 360,height: 28)
        field.setAccessibilityIdentifier("fixture.input");field.setAccessibilityLabel("Input")
        secure.frame = NSRect(x: 30,y: 170,width: 360,height: 28)
        secure.setAccessibilityIdentifier("fixture.password");secure.setAccessibilityLabel("Password")
        button.frame = NSRect(x: 30,y: 110,width: 130,height: 30)
        button.setAccessibilityIdentifier("fixture.apply");button.target=self;button.action=#selector(applied)
        disabled.frame = NSRect(x: 180,y: 110,width: 130,height: 30)
        disabled.setAccessibilityIdentifier("fixture.disabled");disabled.isEnabled=false
        check.frame = NSRect(x: 30,y: 55,width: 220,height: 30)
        check.setAccessibilityIdentifier("fixture.option");check.target=self;check.action=#selector(changed)
        for v in [field,secure,button,disabled,check] { window.contentView!.addSubview(v) }
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps:true)
        _ = Timer.scheduledTimer(withTimeInterval:0.1,repeats:true) { _ in self.save() }
        save()
    }
    @objc func applied() { clicks += 1;save() }
    @objc func changed() { save() }
    func save() {
        let v:[String:Any] = ["pid":ProcessInfo.processInfo.processIdentifier,"clicks":clicks,"text":field.stringValue,"checked":check.state == .on]
        if let d = try? JSONSerialization.data(withJSONObject:v) { try? d.write(to:URL(fileURLWithPath:output),options:.atomic) }
    }
}
let app=NSApplication.shared
let fixture=Fixture()
app.setActivationPolicy(.regular);app.delegate=fixture;app.run()

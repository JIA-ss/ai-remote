#if DEBUG && os(iOS)
import SwiftUI
import UIKit

/// Simulator UI acceptance only (`-ui-chat-fixture`): the production chat view over demo events, with no
/// host. Sending starts a turn running a slow tool; while it runs, sending joins the turn (steer) or, with
/// `-ui-chat-no-steer`, queues; stop ends the turn as interrupted and offers 继续.
public struct PhoneChatFixtureView: View {
    @PhoneViewState<PhoneChatStore> private var chat: PhoneChatStore

    public init() {
        let chat = PhoneChatStore(client: PhoneClient(directory: NSTemporaryDirectory() + "ui-chat-fixture"), host: "UI 测试电脑")
        chat.fixture = true
        let now = Date().timeIntervalSince1970 * 1000
        let session = PhoneChatSession(.object([
            "id": .string("fixture-1"), "name": .string("Claude Code"), "cwd": .string("/Users/me/work/ai-remote"),
            "state": .string("idle"), "by": .object(["device": .string("my-mac")]), "turns": .number(1), "last_activity": .number(now)]))
        chat.sessions = [session, PhoneChatSession(.object([
            "id": .string("fixture-2"), "name": .string("Codex"), "cwd": .string("/Users/me/notes"), "state": .string("parked"),
            "by": .object(["device": .string("iPhone")]), "turns": .number(4), "last_activity": .number(now - 3_600_000)]))]
        chat.runtimes = [
            PhoneChatRuntime(.object(["id": .string("claude"), "name": .string("Claude Code"), "verdict": .string("ok"), "login": .object(["state": .string("logged_in")])])),
            PhoneChatRuntime(.object(["id": .string("codex"), "name": .string("Codex"), "verdict": .string("ok"), "login": .object(["state": .string("logged_out")])])),
            PhoneChatRuntime(.object(["id": .string("gemini"), "name": .string("Gemini CLI"), "verdict": .string("missing")])),
        ]
        chat.newRuntime = "claude"
        func text(_ v: String) -> JSONValue { .string(v) }
        let lines: [[String: JSONValue]] = [
            ["t": text("user"), "turn": .number(0), "text": text("看一下 README 里构建步骤还对不对"), "by": .object(["device": text("my-mac")])],
            ["t": text("turn_start"), "turn": .number(1)],
            ["t": text("thought"), "turn": .number(1), "id": text("th"), "text": text("先读 README，再对照 Cargo.toml 和 tools/ios.sh。")],
            ["t": text("plan"), "turn": .number(1), "entries": .array([
                .object(["content": text("读 README 和构建脚本"), "status": text("completed")]),
                .object(["content": text("对照 ios.sh 的实际步骤"), "status": text("completed")]),
                .object(["content": text("改 README"), "status": text("in_progress")])])],
            ["t": text("tool_call"), "turn": .number(1), "call_id": text("t1"), "title": text("Read README.md"), "kind": text("read"), "status": text("completed")],
            ["t": text("tool_call"), "turn": .number(1), "call_id": text("t3"), "title": text("Read tools/ios.sh"), "kind": text("read"), "status": text("completed")],
            ["t": text("tool_call"), "turn": .number(1), "call_id": text("t2"), "title": text("Bash cargo build -p ar-mobile --release --target aarch64-apple-ios"), "kind": text("execute"),
             "status": text("failed"), "input": .object(["command": text("cargo build -p ar-mobile --release --target aarch64-apple-ios")]),
             "content": .array([.object(["type": text("text"), "text": text("error: the aarch64-apple-ios target may not be installed")])])],
            ["t": text("tool_call"), "turn": .number(1), "call_id": text("t4"), "title": text("Edit README.md"), "kind": text("edit"), "status": text("completed"),
             "content": .array([.object(["type": text("diff"), "path": text("apps/ios/README.md"),
                                         "old": text("## Build\n\nInstall Xcode.\nRun tools/ios.sh framework.\n"),
                                         "new": text("## Build\n\nInstall Xcode.\nRun rustup target add aarch64-apple-ios aarch64-apple-ios-sim.\nRun tools/ios.sh framework.\n")])])],
            ["t": text("message"), "turn": .number(1), "id": text("m"), "text": text("构建步骤基本是对的，只有一处：iOS 需要先 `rustup target add aarch64-apple-ios-sim`，README 里漏了，**已补上**。")],
            ["t": text("question"), "turn": .number(1), "qid": text("q1"), "prompt": text("要不要顺便把 CI 里的 Xcode 版本也写进 README？"), "options": .array([text("要"), text("不用")])],
            ["t": text("usage"), "turn": .number(1), "input_tokens": .number(5200), "output_tokens": .number(800), "context_window": .number(200000), "context_used": .number(46000)],
            ["t": text("turn_end"), "turn": .number(1), "status": text("completed"), "duration_ms": .number(8400)],
            ["t": text("state"), "turn": .number(1), "state": text("idle")],
            // A turn that writes a page, notes and a chart: previews under it (`-ui-chat-fixture` only).
            ["t": text("user"), "turn": .number(1), "text": text("把构建耗时做成一页报告"), "by": .object(["device": text("my-mac")])],
            ["t": text("turn_start"), "turn": .number(2)],
            ["t": text("tool_call"), "turn": .number(2), "call_id": text("w1"), "title": text("Write out/report.html"), "kind": text("edit"), "status": text("completed"),
             "content": .array([.object(["type": text("diff"), "path": text("/Users/me/work/ai-remote/out/report.html"), "new": text("<!doctype html>…")])])],
            ["t": text("tool_call"), "turn": .number(2), "call_id": text("w2"), "title": text("Bash python chart.py"), "kind": text("execute"), "status": text("completed"),
             "locations": .array([text("/Users/me/work/ai-remote/out/chart.png")])],
            ["t": text("message"), "turn": .number(2), "id": text("m2"), "text": text("做好了：[报告](out/report.html)、笔记 `out/notes.md` 和图 `out/chart.png`。")],
            ["t": text("turn_end"), "turn": .number(2), "status": text("completed"), "duration_ms": .number(5100)],
            ["t": text("state"), "turn": .number(2), "state": text("idle")],
        ]
        // The page tries to run a script and fetch an image: neither may happen in the preview.
        chat.fixtureFiles = [
            "/Users/me/work/ai-remote/out/report.html": Data("""
                <!doctype html><html><head><title>Report</title></head>
                <body style="font:16px -apple-system;margin:20px"><h1>构建耗时报告</h1><p id="p">静态内容已显示</p>
                <script>document.getElementById("p").textContent = "脚本运行了";</script>
                <img src="https://example.com/beacon.png" alt="远程图片"></body></html>
                """.utf8),
            "out/notes.md": Data("# 笔记\n\n- **iOS** 构建 4 分 10 秒\n- `cargo build` 2 分钟\n".utf8),
            "/Users/me/work/ai-remote/out/chart.png": UIGraphicsImageRenderer(size: CGSize(width: 64, height: 32)).pngData { ctx in
                for x in 0..<64 {
                    UIColor(red: CGFloat(x) / 64, green: 0.4, blue: 0.7, alpha: 1).setFill()
                    ctx.fill(CGRect(x: x, y: 0, width: 1, height: 32))
                }
            },
        ]
        chat.current = session
        chat.role = "rw"; chat.connected = true; chat.runtimeName = "Claude Code"
        // `-ui-chat-no-steer`: an agent that cannot take a message mid-turn, so sending queues.
        var caps = PhoneChatCaps(); caps.steer = !ProcessInfo.processInfo.arguments.contains("-ui-chat-no-steer")
        chat.caps = caps
        for line in lines { chat.fixtureEvent(line) }
        _chat = State(initialValue: chat)
    }

    public var body: some View { PhoneChatView(chat: chat) {} }
}
#endif

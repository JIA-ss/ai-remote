#if DEBUG && os(iOS)
import SwiftUI

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

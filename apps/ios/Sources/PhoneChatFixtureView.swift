#if DEBUG && os(iOS)
import SwiftUI

/// Simulator UI acceptance only (`-ui-chat-fixture`): the production chat view over demo events, with no
/// host. Sending folds a user line and a running tool in locally; stop ends that turn as interrupted.
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
        let lines: [[String: JSONValue]] = [
            ["t": .string("user"), "turn": .number(0), "text": .string("看一下 README 里构建步骤还对不对"), "by": .object(["device": .string("my-mac")])],
            ["t": .string("turn_start"), "turn": .number(1)],
            ["t": .string("thought"), "turn": .number(1), "id": .string("th"), "text": .string("先读 README，再对照 Cargo.toml 和 tools/ios.sh。")],
            ["t": .string("tool_call"), "turn": .number(1), "call_id": .string("t1"), "title": .string("Read README.md"), "kind": .string("read"), "status": .string("completed")],
            ["t": .string("tool_call"), "turn": .number(1), "call_id": .string("t2"), "title": .string("cargo build -p ar-mobile --release --target aarch64-apple-ios"), "kind": .string("execute"), "status": .string("failed")],
            ["t": .string("message"), "turn": .number(1), "id": .string("m"), "text": .string("构建步骤基本是对的，只有一处：iOS 需要先 `rustup target add aarch64-apple-ios-sim`，README 里漏了。")],
            ["t": .string("turn_end"), "turn": .number(1), "status": .string("completed"), "duration_ms": .number(8400)],
            ["t": .string("state"), "turn": .number(1), "state": .string("idle")],
        ]
        chat.current = session
        chat.role = "rw"; chat.connected = true; chat.runtimeName = "Claude Code"
        for line in lines { chat.fixtureEvent(line) }
        _chat = State(initialValue: chat)
    }

    public var body: some View { PhoneChatView(chat: chat) {} }
}
#endif

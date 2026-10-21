import Foundation
import SwiftUI

/// The coding-agent chats of one host: its sessions, and the one open conversation.
@MainActor @Observable public final class PhoneChatStore {
    public let host: String
    public var sessions: [PhoneChatSession] = []
    public var loading = false
    public var error: String?
    /// The open conversation's session, while one is open.
    public var current: PhoneChatSession?
    public var log = PhoneChatLog()
    /// `rw` for the device that started the session, `ro` (watch only) for every other device.
    public var role: String?
    public var runtimeName = ""
    /// What the agent can do (`caps` of the channel's hello); nil before the channel opens.
    public var caps: PhoneChatCaps?
    /// While a turn runs, a message is added to it (steer) when the agent can take that; this switch
    /// (the composer's chip) sends it after the turn instead.
    public var steerWhileRunning = true
    public var connected = false
    /// Why the conversation's connection ended, when it did; the log stays and can be resumed.
    public var disconnected: String?
    public var draft = ""
    public var sending = false
    /// Starting a new session: the host's agents, and whether it may start any (and why not).
    public var runtimes: [PhoneChatRuntime] = []
    public var canStart = true
    public var startBlocked: String?
    public var showNew = false
    public var newRuntime = ""
    public var newCwd = "~"
    public var newPrompt = ""
    /// A `chat_start` is waiting (for the agent, or for the owner to allow it on the computer).
    public var starting = false
    private let client: PhoneClient
    private var subscription: UInt64?
    private var generation = 0
    #if DEBUG
    /// Simulator UI acceptance (`-ui-chat-fixture`): no host; what would go to it is folded in locally.
    var fixture = false
    private var fixtureSeq = 1000
    func fixtureEvent(_ fields: [String: JSONValue]) {
        fixtureSeq += 1
        var event = fields
        event["seq"] = .number(Double(fixtureSeq)); event["ts"] = .number(Date().timeIntervalSince1970 * 1000)
        log.apply(.object(event))
    }
    #endif

    public init(client: PhoneClient, host: String) {
        self.client = client
        self.host = host
    }

    public var canWrite: Bool { role == "rw" && connected && log.state != "ended" }

    public func loadSessions() async {
        #if DEBUG
        if fixture { return }
        #endif
        loading = true; defer { loading = false }
        do {
            let reply = try await client.call(host: host, tool: "chat_sessions", args: [:])
            sessions = reply["structuredContent"]["sessions"].array.map(PhoneChatSession.init)
                .sorted { $0.lastActivity > $1.lastActivity }
        } catch { self.error = error.localizedDescription }
    }

    public func loadRuntimes() async {
        #if DEBUG
        if fixture { return }
        #endif
        do {
            let reply = try await client.call(host: host, tool: "chat_runtimes", args: [:])
            let report = reply["structuredContent"]
            runtimes = report["runtimes"].array.map(PhoneChatRuntime.init)
            let enabled = report["host"]["enabled"] != .bool(false)
            canStart = report["host"]["can_start"] != .bool(false) && enabled
            startBlocked = canStart ? nil : (report["host"]["reason"].string ?? (enabled ? "这台电脑不能启动智能体" : "这台电脑关闭了编程智能体"))
            if !runtimes.contains(where: { $0.id == newRuntime && $0.usable }) {
                newRuntime = runtimes.first(where: \.usable)?.id ?? ""
            }
        } catch { self.error = error.localizedDescription }
    }

    /// Starts an agent in a folder on the host and opens its conversation. The call waits while the
    /// owner decides on the computer; a refusal comes back as the host's own words.
    public func start() async {
        let cwd = newCwd.trimmingCharacters(in: .whitespacesAndNewlines)
        let prompt = newPrompt.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !newRuntime.isEmpty, !cwd.isEmpty, !starting else { return }
        #if DEBUG
        if fixture { self.error = "UI 测试：没有连接被控端，无法启动。"; return }
        #endif
        starting = true; defer { starting = false }
        var args: [String: JSONValue] = ["runtime": .string(newRuntime), "cwd": .string(cwd), "intent": .string("从 iPhone 启动")]
        if !prompt.isEmpty { args["prompt"] = .string(prompt) }
        do {
            let reply = try await client.call(host: host, tool: "chat_start", args: args)
            let started = reply["structuredContent"]
            await close()
            current = PhoneChatSession(started)
            log = PhoneChatLog()
            showNew = false; newPrompt = ""
            // The ticket in the reply is good once; if it cannot be used, attach asks for a new one.
            if let ticket = started["ticket"].string, await open(ticket: ticket, since: 0) { return }
            await attach(since: 0)
        } catch { self.error = error.localizedDescription }
    }

    public func open(_ session: PhoneChatSession) async {
        await close()
        current = session
        log = PhoneChatLog()
        await attach(since: 0)
    }

    /// Opens the conversation again after its connection ended, asking only for what came after.
    public func resume() async {
        guard current != nil, !connected else { return }
        await attach(since: UInt64(max(log.seq, 0)))
    }

    private func attach(since: UInt64) async {
        guard let session = current else { return }
        disconnected = nil
        do {
            let attached = try await client.call(host: host, tool: "chat_attach", args: ["session": .string(session.id)])
            guard current?.id == session.id, let ticket = attached["structuredContent"]["ticket"].string else { return }
            _ = await open(ticket: ticket, since: since, report: true)
        } catch {
            self.error = error.localizedDescription; disconnected = error.localizedDescription
        }
    }

    /// Opens the chat channel with a ticket; false (and, with `report`, an error shown) when it fails.
    private func open(ticket: String, since: UInt64, report: Bool = false) async -> Bool {
        guard let session = current else { return false }
        generation += 1
        let token = generation
        disconnected = nil
        do {
            // Events arrive off the main thread; one ordered stream keeps the deltas in order.
            let (lines, sink) = AsyncStream<String>.makeStream()
            let listener = StreamListener(event: { line in sink.yield(line) }, ended: { reason in
                sink.yield("\u{0}" + reason)
                sink.finish()
            })
            let view = try await client.chat(host: host, ticket: ticket, since: since, listener: listener)
            guard token == generation else { await client.stop(view.subscription); return true }
            subscription = view.subscription
            let hello = try JSONValue.parse(view.replyJson)
            role = hello["role"].string
            caps = PhoneChatCaps(hello["caps"])
            runtimeName = hello["runtime"]["name"].string ?? session.name
            if let state = hello["state"].string { log.state = state }
            connected = true
            Task { [weak self] in
                for await line in lines {
                    guard let self, token == self.generation else { return }
                    if line.hasPrefix("\u{0}") { self.ended(String(line.dropFirst()), token: token) }
                    else { self.log.apply(line: line) }
                }
            }
            return true
        } catch {
            if report && token == generation { self.error = error.localizedDescription; disconnected = error.localizedDescription }
            return false
        }
    }

    private func ended(_ reason: String, token: Int) {
        guard token == generation else { return }
        connected = false; subscription = nil
        if reason != "stopped" { disconnected = reason == "host disconnected" ? "被控端断开了对话" : reason }
    }

    /// How sending is handled now: nil while nothing runs; `steer` (added to the running turn) or
    /// `queue` (sent after it) while the agent works.
    public var sendMode: String? {
        let mode = phoneSendMode(running: log.busy, caps: caps)
        return mode == "steer" && !steerWhileRunning ? "queue" : mode
    }

    /// Sends the draft: a new turn, or while the agent works, added to its turn or queued after it
    /// (`interrupt`: stop the turn and send this first).
    public func send(interrupt: Bool = false) async {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, canWrite, !sending else { return }
        sending = true; defer { sending = false }
        let mode = interrupt && log.busy ? "interrupt" : sendMode
        if await command(phoneInputCommand(id: UUID().uuidString, text: text, mode: mode)),
           draft.trimmingCharacters(in: .whitespacesAndNewlines) == text {
            draft = ""
        }
    }

    /// Stops the running turn; the session stays and takes the next message.
    /// A watch-only conversation (an older host) still stops through the `chat_interrupt` tool.
    public func interrupt() async {
        guard let session = current else { return }
        if canWrite || fixtureMode { _ = await command(["interrupt": .object([:])]); return }
        do { _ = try await client.call(host: host, tool: "chat_interrupt", args: ["session": .string(session.id)]) }
        catch { self.error = error.localizedDescription }
    }

    /// Takes a queued message back.
    public func dequeue(_ id: String) async {
        _ = await command(["dequeue": .object(["id": .string(id)])])
    }

    /// Changes a queued message: taken back and queued again with the new text (at the end).
    public func editQueued(_ id: String, text: String) async {
        let value = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty else { return }
        for c in phoneEditQueued(queuedID: id, newID: UUID().uuidString, text: value) { guard await command(c) else { return } }
    }

    /// Sends a queued message into the running turn now instead of after it.
    public func steerQueued(_ queued: PhoneQueued) async {
        guard await command(["dequeue": .object(["id": .string(queued.id)])]) else { return }
        _ = await command(phoneInputCommand(id: UUID().uuidString, text: queued.text, mode: "steer"))
    }

    /// Answers the agent's question.
    public func answer(_ qid: String, text: String) async {
        let value = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty else { return }
        if await command(["answer": .object(["qid": .string(qid), "text": .string(value)])]) { log.markAnswered(qid, answer: value) }
    }

    /// After "你停止了这一轮": asks the agent to pick up where it stopped.
    public func continueStopped() async {
        _ = await command(phoneInputCommand(id: UUID().uuidString, text: phoneContinuePrompt))
    }

    private var fixtureMode: Bool {
        #if DEBUG
        fixture
        #else
        false
        #endif
    }

    /// One command on the open chat; false (with the error shown) when it did not go out.
    private func command(_ command: [String: JSONValue]) async -> Bool {
        #if DEBUG
        if fixture { fixtureCommand(command); return true }
        #endif
        guard let id = subscription else { self.error = "对话已断开"; return false }
        do { try await client.chatSend(id, command); return true }
        catch { self.error = error.localizedDescription; return false }
    }

    #if DEBUG
    /// What a host would answer to a command, folded in locally for the UI fixture: a new message
    /// starts a turn running a slow tool, steer joins the running turn, queue waits, stop ends the turn.
    private func fixtureCommand(_ command: [String: JSONValue]) {
        let n = Double(log.turns.last?.n ?? 0)
        let me: JSONValue = .object(["device": .string("ui-fixture")])
        if let input = command["input"] {
            let text = input["text"].string ?? ""
            switch input["mode"].string {
            case "steer" where log.busy:
                fixtureEvent(["t": .string("user"), "turn": .number(n), "text": .string(text), "by": me, "steer": .bool(true)])
            case nil where log.busy:
                let queued = log.queued.map { JSONValue.object(["id": .string($0.id), "text": .string($0.text)]) }
                fixtureEvent(["t": .string("state"), "turn": .number(n), "state": .string("running"),
                              "queued": .array(queued + [.object(["id": input["id"], "text": .string(text)])])])
            default:
                fixtureEvent(["t": .string("user"), "turn": .number(n), "text": .string(text), "by": me])
                fixtureEvent(["t": .string("turn_start"), "turn": .number(n + 1)])
                fixtureEvent(["t": .string("state"), "turn": .number(n + 1), "state": .string("running")])
                fixtureEvent(["t": .string("tool_call"), "turn": .number(n + 1), "call_id": .string("f\(n)"), "title": .string("Bash sleep 600"),
                              "kind": .string("execute"), "status": .string("in_progress")])
            }
        } else if command["interrupt"] != nil {
            fixtureEvent(["t": .string("turn_end"), "turn": .number(n), "status": .string("interrupted"), "duration_ms": .number(1200)])
            let queued = log.queued.map { JSONValue.object(["id": .string($0.id), "text": .string($0.text)]) }
            fixtureEvent(["t": .string("state"), "turn": .number(n), "state": .string("idle"), "queued": .array(queued)])
        } else if let dequeue = command["dequeue"] {
            let queued = log.queued.filter { $0.id != dequeue["id"].string }.map { JSONValue.object(["id": .string($0.id), "text": .string($0.text)]) }
            fixtureEvent(["t": .string("state"), "turn": .number(n), "state": .string(log.state), "queued": .array(queued)])
        }
    }
    #endif

    /// Stops watching (the agent keeps its session on the host).
    public func close() async {
        generation += 1
        if let id = subscription { await client.stop(id) }
        subscription = nil; connected = false; role = nil; disconnected = nil
    }

    /// Back to the session list.
    public func leave() async {
        await close()
        current = nil; log = PhoneChatLog(); draft = ""
        await loadSessions()
    }
}

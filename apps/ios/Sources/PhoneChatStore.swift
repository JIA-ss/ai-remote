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
    /// The agent's id and protocol from the channel's hello (`runtime.id`, `runtime.protocol`), and the
    /// session that hello was for; before it arrives the session's own runtime id and name decide.
    public var runtimeID: String?
    public var runtimeProtocol: String?
    public var runtimeFor: String?
    /// Codex turns whose work the person opened ("Worked for …"), by session and turn.
    public var codexOpened: Set<String> = []
    /// What was chosen and typed in Codex's question card, by request (see `codexPending`); a request
    /// that is over (answered here or elsewhere, closed, expired, its turn ended) leaves no draft.
    public var codexDrafts: [String: PhoneCodexDraft] = [:]
    /// The request whose card the person closed (which stops the turn): not asked again meanwhile.
    public var codexClosed: String?
    /// The composer takes the keyboard when it next shows (after a question card it stood in for).
    public var focusComposer = false
    /// What the agent can do (`caps` of the channel's hello); nil before the channel opens.
    public var caps: PhoneChatCaps?
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
    /// Images to send with the next message (at most four; scaled before they are attached).
    public var attachments: [PhoneImage] = []
    /// Why an image was not attached, shown under the composer until the next change.
    public var attachNote: String?
    /// Messages this phone sent that the conversation does not show yet (sending / not delivered).
    public var outgoing: [PhoneOutgoing] = []
    /// The host answers every input with an `ack` (or a refusal naming it): the hello's `acks`.
    public var hostAcks = false
    /// Pictures of the images this phone sent, by message id, for its own bubbles (this run only).
    public var sentImages: [String: [PhoneImage]] = [:]
    /// The host user's home as last seen by a listing of `~` (shortens paths to `~`).
    public var home: String?
    private let client: PhoneClient
    private var subscription: UInt64?
    private var generation = 0
    /// Requests waiting for the reply with their `rid` (`files`).
    private var waiting: [String: CheckedContinuation<JSONValue, Error>] = [:]
    private var nextRid = 0
    /// Preview bytes of this conversation's files, fetched once each (a failure is not kept).
    private var previews: [String: Task<Data, Error>] = [:]
    #if DEBUG
    /// Simulator UI acceptance (`-ui-chat-fixture`): no host; what would go to it is folded in locally.
    var fixture = false
    var fixtureRefuse = false
    /// The fixture shows a Codex session: a new message starts a Codex-shaped turn.
    var fixtureCodex = false
    /// Answers to the fixture's Codex questions so far, by request and question (the host combines them).
    var fixtureAnswers: [String: [String: String]] = [:]
    private var fixtureSeq = 1000
    func fixtureEvent(_ fields: [String: JSONValue]) {
        fixtureSeq += 1
        var event = fields
        event["seq"] = .number(Double(fixtureSeq))
        if event["ts"] == nil { event["ts"] = .number(Date().timeIntervalSince1970 * 1000) }
        log.apply(.object(event))
        forgetDrafts()
    }
    #endif

    public init(client: PhoneClient, host: String) {
        self.client = client
        self.host = host
        home = (UserDefaults.standard.dictionary(forKey: Self.homeKey) as? [String: String])?[host]
    }

    // MARK: Folders on the host

    private static let recentKey = "ai-remote.chat.cwd"
    private static let homeKey = "ai-remote.chat.home"

    /// Folders agents were last started in on this host, newest first (kept on this phone).
    public var recentFolders: [String] {
        (UserDefaults.standard.dictionary(forKey: Self.recentKey) as? [String: [String]])?[host] ?? []
    }
    public func rememberFolder(_ cwd: String) {
        var all = (UserDefaults.standard.dictionary(forKey: Self.recentKey) as? [String: [String]]) ?? [:]
        all[host] = Array(([cwd] + (all[host] ?? []).filter { !phoneSameFolder($0, cwd) }).prefix(6))
        UserDefaults.standard.set(all, forKey: Self.recentKey)
    }
    /// The folders to offer first: recent ones, then those of the host's sessions.
    public var folderChoices: [String] { phoneFolderChoices(recent: recentFolders, sessions: sessions.map(\.cwd)) }

    /// A folder on the host and its subfolders (`files_list`, the first page: folders come first).
    /// Listing `~` also tells where the host user's home is.
    public func browse(_ path: String) async throws -> (path: String, folders: [String], more: Bool) {
        #if DEBUG
        if fixture { return try fixtureBrowse(path) }
        #endif
        let reply = try await client.call(host: host, tool: "files_list", args: ["path": .string(path)])
        let listed = PhoneChatStore.firstLine(reply)
        let shown = listed["path"].string ?? path
        if path.trimmingCharacters(in: .whitespaces) == "~", home != shown {
            home = shown
            var all = (UserDefaults.standard.dictionary(forKey: Self.homeKey) as? [String: String]) ?? [:]
            all[host] = shown
            UserDefaults.standard.set(all, forKey: Self.homeKey)
        }
        return (shown, phoneListedFolders(listed["entries"].array, query: ""), listed["next_offset"].number != nil || listed["more"] == .bool(true))
    }

    /// A tool's answer: its structured content, or the JSON line in its first text block.
    static func firstLine(_ reply: JSONValue) -> JSONValue {
        if case .object = reply["structuredContent"] { return reply["structuredContent"] }
        let text = reply["content"].array.first?["text"].string ?? "{}"
        return (try? JSONValue.parse(String(text.split(separator: "\n", maxSplits: 1).first ?? "{}"))) ?? .null
    }

    // MARK: Previews

    /// The bytes of a file this conversation is about (fetched once; `fresh` asks again).
    public func preview(_ path: String, fresh: Bool = false) async throws -> Data {
        guard let session = current else { throw NSError(domain: "ai-remote", code: 4, userInfo: [NSLocalizedDescriptionKey: "没有打开的会话"]) }
        if !fresh, let task = previews[path] { return try await task.value }
        let client = client, host = host
        #if DEBUG
        let fixture = fixture
        #endif
        let task = Task<Data, Error> {
            #if DEBUG
            if fixture { return try await PhoneChatStore.fixturePreview(path) }
            #endif
            return try await client.previewFile(host: host, session: session.id, path: path)
        }
        previews[path] = task
        do { return try await task.value } catch {
            if previews[path] == task { previews[path] = nil }
            throw error
        }
    }

    public var canWrite: Bool { role == "rw" && connected && log.state != "ended" }

    /// The open conversation is with Codex: it is laid out the way Codex's own app shows one.
    public var isCodex: Bool {
        guard let session = current else { return false }
        if runtimeFor == session.id {
            return phoneIsCodexRuntime(id: runtimeID, name: runtimeName.isEmpty ? session.name : runtimeName, protocolName: runtimeProtocol)
        }
        return phoneIsCodexRuntime(id: session.runtime, name: session.name, protocolName: nil)
    }

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
            rememberFolder(cwd)
            await close()
            current = PhoneChatSession(started)
            log = PhoneChatLog(); outgoing = []; previews = [:]
            showNew = false; newPrompt = ""
            // The ticket in the reply is good once; if it cannot be used, attach asks for a new one.
            if let ticket = started["ticket"].string, await open(ticket: ticket, since: 0) { return }
            await attach(since: 0)
        } catch { self.error = error.localizedDescription }
    }

    public func open(_ session: PhoneChatSession) async {
        await close()
        current = session
        log = PhoneChatLog(); outgoing = []; previews = [:]
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
            runtimeID = hello["runtime"]["id"].string
            runtimeProtocol = hello["runtime"]["protocol"].string
            runtimeFor = session.id
            hostAcks = hello["acks"] == .bool(true)
            if case .array(let list) = hello["commands"] { log.knownCommands(list) }
            if case .bool(let b) = hello["images"] { log.images = b }
            if let state = hello["state"].string { log.state = state }
            connected = true
            Task { [weak self] in
                for await line in lines {
                    guard let self, token == self.generation else { return }
                    if line.hasPrefix("\u{0}") { self.ended(String(line.dropFirst()), token: token) }
                    else { self.received(line) }
                }
            }
            // Messages that got no answer in time count as not delivered.
            Task { [weak self] in
                while true {
                    try? await Task.sleep(for: .seconds(2))
                    guard let self, token == self.generation, self.connected else { return }
                    self.settle()
                }
            }
            return true
        } catch {
            if report && token == generation { self.error = error.localizedDescription; disconnected = error.localizedDescription }
            return false
        }
    }

    /// One line from the host: an event, the agent's commands, a reply to this phone, or a refusal.
    func received(_ line: String) {
        guard let m = try? JSONValue.parse(line) else { return }
        switch phoneChatLineKind(m) {
        case "event"?:
            log.apply(m)
            forgetDrafts()
        case "commands"?: log.knownCommands(m["commands"].array)
        case "reply"?:
            if let rid = m["rid"].string ?? m["rid"].number.map({ String(Int($0)) }), let waiter = waiting.removeValue(forKey: rid) {
                if m["t"].string == "error" {
                    waiter.resume(throwing: NSError(domain: "ai-remote", code: 5, userInfo: [NSLocalizedDescriptionKey: m["text"].string ?? "被控端拒绝了"]))
                } else { waiter.resume(returning: m) }
            } else { outgoing = phoneOutgoingReply(outgoing, m) }
        case "notice"?:
            log.apply(.object(["t": .string("notice"), "seq": .number(-1), "level": .string("error"), "code": m["code"], "text": m["text"]]))
        default: break
        }
        settle()
    }

    private func settle() {
        let next = phoneSettleOutgoing(outgoing, log: log, now: Date().timeIntervalSince1970 * 1000, hostAcks: hostAcks)
        if next != outgoing { outgoing = next }
    }

    private func ended(_ reason: String, token: Int) {
        guard token == generation else { return }
        outgoing = phoneOutgoingDisconnected(outgoing)
        failWaiting()
        connected = false; subscription = nil
        if reason != "stopped" { disconnected = reason == "host disconnected" ? "被控端断开了对话" : reason }
    }

    /// How sending is handled now: nil while nothing runs; while the agent works, `steer` (it joins
    /// the running turn, as the agent itself does) or `queue` (sent after the turn) by its caps.
    public var sendMode: String? { phoneSendMode(running: log.busy, caps: caps) }

    /// Sends the draft: a new turn, or while the agent works, joined to its turn or queued after it as
    /// the agent allows (`interrupt`: stop the turn and send this first).
    public func send(interrupt: Bool = false) async {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        let images = attachments
        guard !text.isEmpty || !images.isEmpty, canWrite, !sending else { return }
        sending = true; defer { sending = false }
        let mode = interrupt && log.busy ? "interrupt" : sendMode
        let o = PhoneOutgoing(id: UUID().uuidString, text: text, mode: mode, at: Date().timeIntervalSince1970 * 1000, images: images)
        // Shown at once as on its way; it leaves when the conversation shows it.
        draft = ""; attachments = []; attachNote = nil
        if !images.isEmpty { sentImages[o.id] = images }
        outgoing.append(o)
        await deliver(o)
    }

    private func deliver(_ o: PhoneOutgoing) async {
        if !(await command(phoneInputCommand(id: o.id, text: o.text, mode: o.mode, images: o.images), report: false)),
           let i = outgoing.firstIndex(where: { $0.id == o.id }) {
            outgoing[i].state = "failed"; outgoing[i].error = "disconnected"
        }
        settle()
    }

    /// Sends a message that did not arrive again (the same id unless the host refused it).
    public func retry(_ o: PhoneOutgoing) async {
        guard let i = outgoing.firstIndex(where: { $0.id == o.id }), canWrite else { return }
        let again = phoneRetryOutgoing(outgoing[i], newID: UUID().uuidString, now: Date().timeIntervalSince1970 * 1000)
        if again.id != o.id, let images = sentImages[o.id] { sentImages[again.id] = images }
        outgoing[i] = again
        await deliver(again)
    }

    /// Forgets a message that did not arrive; its text goes back to the composer when that is empty.
    public func discard(_ o: PhoneOutgoing) {
        outgoing.removeAll { $0.id == o.id }
        if draft.isEmpty { draft = o.text }
        if attachments.isEmpty { attachments = o.images }
    }

    // MARK: The composer's menus and images

    /// The agent's commands and skills for the `/` menu.
    public var commands: [PhoneSlashCommand] { log.commands ?? [] }
    /// Whether the agent takes images: what it said last, else its runtime's caps.
    public var takesImages: Bool { log.images ?? caps?.images ?? false }

    /// Files in the session's folder whose path contains every word of `query`, for the `@` menu.
    public func files(_ query: String, limit: Int = 50) async throws -> [String] {
        #if DEBUG
        if fixture { return phoneRankFiles(Self.fixtureFiles.filter { f in query.split(separator: " ").allSatisfy { f.lowercased().contains($0.lowercased()) } }, query) }
        #endif
        guard let id = subscription else { throw NSError(domain: "ai-remote", code: 3, userInfo: [NSLocalizedDescriptionKey: "对话已断开"]) }
        nextRid += 1
        let rid = "p\(nextRid)"
        let reply: JSONValue = try await withCheckedThrowingContinuation { (k: CheckedContinuation<JSONValue, Error>) in
            self.waiting[rid] = k
            let client = self.client
            Task { @MainActor in
                do { try await client.chatSend(id, ["files": .object(["rid": .string(rid), "query": .string(query), "limit": .number(Double(limit))])]) }
                catch { self.waiting.removeValue(forKey: rid)?.resume(throwing: error) }
            }
            Task { @MainActor in
                try? await Task.sleep(for: .seconds(10))
                self.waiting.removeValue(forKey: rid)?.resume(throwing: NSError(domain: "ai-remote", code: 6, userInfo: [NSLocalizedDescriptionKey: "被控端没有回应"]))
            }
        }
        return phoneRankFiles(reply["files"].array.compactMap(\.string), query)
    }

    private func failWaiting() {
        let all = waiting; waiting = [:]
        for (_, k) in all { k.resume(throwing: NSError(domain: "ai-remote", code: 3, userInfo: [NSLocalizedDescriptionKey: "对话已断开"])) }
    }

    /// Attaches an image (already encoded and scaled); says why when the host would refuse it.
    public func attach(_ image: PhoneImage) {
        switch phoneImageProblem(mime: image.mime, bytes: image.data.count, have: attachments.count) {
        case "count"?: attachNote = "一条消息最多带 \(phoneMaxImages) 张图片"
        case "type"?: attachNote = "只能发送 PNG、JPEG、GIF 或 WebP 图片"
        case "size"?: attachNote = "图片太大（压缩后仍超过 \(phoneSizeLabel(phoneMaxImageBytes))）"
        default: attachments.append(image); attachNote = nil
        }
    }
    public func detach(_ id: String) { attachments.removeAll { $0.id == id } }

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

    /// The Codex questions waiting for an answer, in a card in place of the composer, with the key
    /// their draft is kept under (session, turn and request: a request belongs to its turn).
    public var codexPending: (id: String, request: PhoneCodexRequest)? {
        guard isCodex, let session = current, let req = phonePendingRequest(log.turns) else { return nil }
        let id = "\(session.id):\(req.turn):\(req.request ?? "")"
        return id == codexClosed ? nil : (id, req)
    }

    /// Forgets the drafts of requests that are no longer waiting.
    private func forgetDrafts() {
        guard !codexDrafts.isEmpty else { return }
        let id = codexPending?.id
        if codexDrafts.keys.contains(where: { $0 != id }) { codexDrafts = codexDrafts.filter { $0.key == id } }
    }

    /// Answers a Codex request: one `answer` per question (an empty text skips it; the host combines
    /// them and answers Codex once the request is complete).
    public func answerRequest(_ req: PhoneCodexRequest, id: String, answers: [(qid: String, text: String)]) async {
        for a in answers {
            guard await command(["answer": .object(["qid": .string(a.qid), "text": .string(a.text)])]) else { return }
            log.markAnswered(a.qid, answer: a.text, request: req.request, turn: req.turn)
        }
        codexDrafts[id] = nil
        focusComposer = canWrite
    }

    /// Closes a Codex request's card: Codex waits on the answer, so the turn stops.
    public func dismissRequest(id: String) async {
        codexClosed = id
        codexDrafts[id] = nil
        focusComposer = canWrite
        await interrupt()
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
    private func command(_ command: [String: JSONValue], report: Bool = true) async -> Bool {
        #if DEBUG
        if fixture { fixtureCommand(command); return true }
        #endif
        guard let id = subscription else { if report { self.error = "对话已断开" }; return false }
        do { try await client.chatSend(id, command); return true }
        catch { if report { self.error = error.localizedDescription }; return false }
    }

    #if DEBUG
    /// What a host would answer to a command, folded in locally for the UI fixture: a new message
    /// starts a turn running a slow tool, steer joins the running turn, queue waits, stop ends the turn.
    private func fixtureCommand(_ command: [String: JSONValue]) {
        let n = Double(log.turns.last?.n ?? 0)
        let me: JSONValue = .object(["device": .string("ui-fixture")])
        if let input = command["input"] {
            let text = input["text"].string ?? ""
            let attachments: JSONValue = .array(input["attachments"].array.map { a in
                .object(["name": a["name"], "mime": a["mime"], "bytes": .number(Double(Data(base64Encoded: a["data"].string ?? "")?.count ?? 0))])
            })
            // `-ui-chat-unacked`: the first message is refused, so the bubble offers to send it again.
            if fixtureRefuse { fixtureRefuse = false; received(JSONValue.object(["t": .string("error"), "code": .string("refused"), "input_id": input["id"], "text": .string("UI 测试：被控端拒收了这条消息")]).text); return }
            received(JSONValue.object(["t": .string("ack"), "input_id": input["id"]]).text)
            switch input["mode"].string {
            case "steer" where log.busy:
                fixtureEvent(["t": .string("user"), "turn": .number(n), "text": .string(text), "by": me, "steer": .bool(true), "input_id": input["id"], "attachments": attachments])
            case nil where log.busy:
                let queued = log.queued.map { JSONValue.object(["id": .string($0.id), "text": .string($0.text)]) }
                fixtureEvent(["t": .string("state"), "turn": .number(n), "state": .string("running"),
                              "queued": .array(queued + [.object(["id": input["id"], "text": .string(text)])])])
                settle()
            default:
                fixtureEvent(["t": .string("user"), "turn": .number(n), "text": .string(text), "by": me, "input_id": input["id"], "attachments": attachments])
                fixtureEvent(["t": .string("turn_start"), "turn": .number(n + 1)])
                fixtureEvent(["t": .string("state"), "turn": .number(n + 1), "state": .string("running")])
                if fixtureCodex {
                    fixtureCodexTurn(n + 1)
                } else {
                    fixtureEvent(["t": .string("tool_call"), "turn": .number(n + 1), "call_id": .string("f\(n)"), "title": .string("Bash sleep 600"),
                                  "kind": .string("execute"), "status": .string("in_progress")])
                }
            }
        } else if let answer = command["answer"] {
            fixtureAnswer(answer, turn: n)
        } else if command["interrupt"] != nil {
            // A stopped turn closes the questions it asked, as the host does.
            for request in Set(log.turns.last?.blocks.compactMap { b -> String? in if case .question(let q) = b, !q.answered { q.request } else { nil } } ?? []) {
                fixtureEvent(["t": .string("question_done"), "turn": .number(n), "request": .string(request), "how": .string("dismissed")])
            }
            fixtureEvent(["t": .string("turn_end"), "turn": .number(n), "status": .string("interrupted"), "duration_ms": .number(1200)])
            let queued = log.queued.map { JSONValue.object(["id": .string($0.id), "text": .string($0.text)]) }
            fixtureEvent(["t": .string("state"), "turn": .number(n), "state": .string("idle"), "queued": .array(queued)])
        } else if let dequeue = command["dequeue"] {
            let queued = log.queued.filter { $0.id != dequeue["id"].string }.map { JSONValue.object(["id": .string($0.id), "text": .string($0.text)]) }
            fixtureEvent(["t": .string("state"), "turn": .number(n), "state": .string(log.state), "queued": .array(queued)])
        }
    }
    #endif

    #if DEBUG
    static let fixtureFiles = ["README.md", "apps/ios/README.md", "docs/report.html", "docs/notes.md", "out/chart.png", "tools/ios.sh", "Cargo.toml"]

    /// A small folder tree for the fixture's folder picker.
    func fixtureBrowse(_ path: String) throws -> (path: String, folders: [String], more: Bool) {
        let tree: [String: [String]] = ["/Users/me": ["Desktop", "notes", "work"], "/Users/me/work": ["ai-remote", "site"],
                                        "/Users/me/work/ai-remote": ["apps", "crates", "docs"], "/Users/me/notes": [], "/Users/me/work/site": []]
        var p = phoneTrimFolder(path)
        if p == "~" || p.hasPrefix("~/") { p = "/Users/me" + p.dropFirst() }
        if path.trimmingCharacters(in: .whitespaces) == "~" { home = "/Users/me" }
        guard let folders = tree[p] else { throw NSError(domain: "ai-remote", code: 7, userInfo: [NSLocalizedDescriptionKey: "\(p) does not exist"]) }
        return (p, folders, false)
    }

    /// What a host would send for the fixture's files: a page, a note and a chart.
    static func fixturePreview(_ path: String) async throws -> Data {
        try await Task.sleep(for: .milliseconds(150))
        switch phonePreviewKind(path) {
        case "html"?:
            return Data(("<!doctype html><html><head><title>报告</title><style>body{font:16px -apple-system;margin:24px}h1{color:#2563eb}"
                + ".bar{height:18px;background:#16a34a;border-radius:4px}</style></head>"
                + "<body><h1>构建报告</h1><p>iOS 构建通过，用时 <b>4 分 12 秒</b>。</p><div class=bar style=width:70%></div>"
                + "<p id=js>脚本没有运行</p><script>document.getElementById('js').textContent='脚本运行了'</script><img src=https://example.com/x.png alt=外部图片></body></html>").utf8)
        case "markdown"?:
            return Data("""
                # 构建说明

                先装 **Xcode**，再运行：

                ```sh
                tools/ios.sh framework
                ```

                - 第一步：`rustup target add aarch64-apple-ios-sim`
                - 第二步：`xcodegen generate`

                > 模拟器里不能拍照。

                | 平台 | 状态 |
                |---|---|
                | iOS | 通过 |
                """.utf8)
        default:
            return phoneFixturePNG(width: 320, height: 200)
        }
    }
    #endif

    /// Stops watching (the agent keeps its session on the host).
    public func close() async {
        generation += 1
        failWaiting()
        if let id = subscription { await client.stop(id) }
        subscription = nil; connected = false; role = nil; disconnected = nil
    }

    /// Back to the session list.
    public func leave() async {
        await close()
        current = nil; log = PhoneChatLog(); draft = ""; attachments = []; outgoing = []; previews = [:]
        await loadSessions()
    }
}

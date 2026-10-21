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
    public var connected = false
    /// Why the conversation's connection ended, when it did; the log stays and can be resumed.
    public var disconnected: String?
    public var draft = ""
    public var sending = false
    private let client: PhoneClient
    private var subscription: UInt64?
    private var generation = 0

    public init(client: PhoneClient, host: String) {
        self.client = client
        self.host = host
    }

    public var canWrite: Bool { role == "rw" && connected && log.state != "ended" }

    public func loadSessions() async {
        loading = true; defer { loading = false }
        do {
            let reply = try await client.call(host: host, tool: "chat_sessions", args: [:])
            sessions = reply["structuredContent"]["sessions"].array.map(PhoneChatSession.init)
                .sorted { $0.lastActivity > $1.lastActivity }
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
        generation += 1
        let token = generation
        disconnected = nil
        do {
            let attached = try await client.call(host: host, tool: "chat_attach", args: ["session": .string(session.id)])
            let ticket = attached["structuredContent"]["ticket"].string ?? ""
            guard token == generation, !ticket.isEmpty else { return }
            // Events arrive off the main thread; one ordered stream keeps the deltas in order.
            let (lines, sink) = AsyncStream<String>.makeStream()
            let listener = StreamListener(event: { line in sink.yield(line) }, ended: { reason in
                sink.yield("\u{0}" + reason)
                sink.finish()
            })
            let view = try await client.chat(host: host, ticket: ticket, since: since, listener: listener)
            guard token == generation else { await client.stop(view.subscription); return }
            subscription = view.subscription
            let hello = try JSONValue.parse(view.replyJson)
            role = hello["role"].string
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
        } catch {
            if token == generation { self.error = error.localizedDescription; disconnected = error.localizedDescription }
        }
    }

    private func ended(_ reason: String, token: Int) {
        guard token == generation else { return }
        connected = false; subscription = nil
        if reason != "stopped" { disconnected = reason == "host disconnected" ? "被控端断开了对话" : reason }
    }

    /// Sends the draft as the next message; while the agent works it waits in the host's queue.
    public func send() async {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, canWrite, let id = subscription, !sending else { return }
        sending = true; defer { sending = false }
        do {
            try await client.chatSend(id, ["input": .object(["id": .string(UUID().uuidString), "text": .string(text), "mode": .string("queue")])])
            if draft.trimmingCharacters(in: .whitespacesAndNewlines) == text { draft = "" }
        } catch { self.error = error.localizedDescription }
    }

    /// Stops the running turn; the session stays and takes the next message.
    public func interrupt() async {
        guard canWrite, let id = subscription else { return }
        do { try await client.chatSend(id, ["interrupt": .object([:])]) }
        catch { self.error = error.localizedDescription }
    }

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

import Foundation
import AIRemoteCore

public final class StreamListener: MobileStream, @unchecked Sendable {
    private let onEvent: @Sendable (String) -> Void
    private let onFrame: @Sendable (Data) -> Void
    private let onEnd: @Sendable (String) -> Void
    public init(event: @escaping @Sendable (String) -> Void = { _ in }, frame: @escaping @Sendable (Data) -> Void = { _ in }, ended: @escaping @Sendable (String) -> Void = { _ in }) {
        onEvent = event; onFrame = frame; onEnd = ended
    }
    public func event(json: String) { onEvent(json) }
    public func frame(bytes: Data) { onFrame(bytes) }
    public func ended(reason: String) { onEnd(reason) }
}

public actor PhoneClient {
    private var controller: MobileController?
    private var starting: Task<MobileController, Error>?
    private var generation = 0
    private let directory: String
    public init(directory: String) { self.directory = directory }
    public func start() async throws {
        if controller != nil { return }
        let token = generation
        let task: Task<MobileController, Error>
        if let existing = starting { task = existing }
        else {
            let path = directory
            task = Task { try await MobileController.connect(directory: path) }
            starting = task
        }
        do {
            let connected = try await task.value
            guard token == generation else { await connected.close(); throw CancellationError() }
            controller = connected; starting = nil
        } catch { if token == generation { starting = nil }; throw error }
    }
    public func close() async {
        generation += 1
        let pending = starting; starting = nil
        let old = controller; controller = nil
        if let pending { pending.cancel(); if let connected = try? await pending.value { await connected.close() } }
        await old?.close()
    }
    private func ready() throws -> MobileController {
        guard let controller else { throw NSError(domain: "ai-remote", code: 1, userInfo: [NSLocalizedDescriptionKey: "控制端尚未连接"]) }
        return controller
    }
    public func request(_ object: [String: JSONValue]) async throws -> JSONValue {
        let reply = try await ready().request(requestJson: JSONValue.object(object).text)
        return try JSONValue.parse(reply)
    }
    public func call(host: String, tool: String, args: [String: JSONValue]) async throws -> JSONValue {
        let reply = try await request(["op": .string("call"), "host": .string(host), "tool": .string(tool), "args": .object(args), "person": .bool(true)])
        if reply["isError"] == .bool(true) {
            throw NSError(domain: "ai-remote", code: 2, userInfo: [NSLocalizedDescriptionKey: reply["content"].array.first?["text"].string ?? "被控端拒绝了操作"])
        }
        return reply
    }
    public func turn(host: String, action: String) async throws -> JSONValue {
        try await request(["op": .string("channel"), "host": .string(host), "request": .object(["turn": .object(["action": .string(action), "agent_may_watch": .bool(true)])])])
    }
    public func events(host: String, listener: StreamListener) async throws -> UInt64 { try await ready().watchEvents(host: host, listener: listener) }
    public func video(host: String, listener: StreamListener) async throws -> MobileView {
        try await ready().watchScreen(host: host, monitor: nil, maxFps: 15, maxLongEdge: 1280, listener: listener)
    }
    public func stop(_ id: UInt64) async { await controller?.stopSubscription(subscription: id) }
    public func keyframe(_ id: UInt64) async { await controller?.requestKeyframe(subscription: id) }
}

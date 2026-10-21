import Foundation
import SwiftUI
import CoreImage
import AIRemoteCore

@MainActor @Observable public final class PhoneStore {
    public var hosts: [PhoneHost] = []
    public var selected: PhoneHost?
    /// The selected host as last reported while connected: its route (direct or relayed) can change.
    public var route: PhoneHost?
    public var error: String?
    public var pairingCode = ""
    public var safetyCode: String?
    public var loading = false
    public var pairSheet = false
    public var showActivity = false
    public var connected = false
    public var image: CGImage?
    public var screen: ScreenGeometry?
    /// The host's pointer, drawn by this app over the picture (the host leaves it out of the picture
    /// when it can). `nil`: in the picture, or not known yet.
    public var pointer: PhonePointer?
    private var pointerShapes: [UInt32: PointerShape] = [:]
    public var level: String?
    /// The host's owner trusts this device with everything: every tool, no approvals, whatever `level`.
    public var bypass = false
    public var driver = "agent"
    public var driverDevice = ""
    public var ownName = ""
    public var pending: [JSONValue] = []
    public var activity: [JSONValue] = []
    public var device = ""
    public var text = ""
    private let client: PhoneClient
    private var eventID: UInt64?
    private var videoID: UInt64?
    private var decoder: H264Decoder?
    private var imageRelay: PhonePictureRelay?
    private var keyframePending = false
    private var generation = 0
    private var inputTail: Task<Void, Never>?
    private var routeWatch: Task<Void, Never>?
    private var suspending: Task<Void, Never>?
    private var lifecycleGeneration = 0
    private var active = true
    private var leaving = false
    private var hasDriver: Bool { driver == "person" && driverDevice == ownName && !ownName.isEmpty }
    public var ownsControls: Bool { hasDriver && active && !leaving }
    public var mayControl: Bool { bypass || level == "control" || level == "admin" }

    public init(directory: String? = nil) {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        let path = directory ?? base.appendingPathComponent("ai-remote").path
        client = PhoneClient(directory: path)
    }
    public func resume() async {
        active = true
        if let suspending { await suspending.value }
        let token = lifecycleGeneration
        loading = true; defer { loading = false }
        do {
            try await client.start()
            let status = try await client.request(["op": .string("status")])
            device = status["device"].string ?? ""
            try await refresh()
        } catch is CancellationError {
            // An older startup may finish after a background transition.
        } catch { if active && token == lifecycleGeneration { self.error = error.localizedDescription } }
    }
    public func refresh() async throws {
        let result = try await client.request(["op": .string("hosts")])
        hosts = result.array.map(PhoneHost.init)
    }
    public func pair() async {
        let ticket = pairingCode.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !ticket.isEmpty else { return }
        loading = true; defer { loading = false }
        do {
            let result = try await client.request(["op": .string("pair"), "ticket": .string(ticket)])
            safetyCode = result["sas"].string
            pairingCode = ""
            try await refresh()
        } catch { self.error = error.localizedDescription }
    }
    public func connect(_ host: PhoneHost) async {
        await leaveSession()
        selected = host; generation += 1
        let token = generation
        do {
            let access = try await client.request(["op": .string("channel"), "host": .string(host.name), "request": .object(["access": .bool(true)])])
            guard token == generation else { return }
            ownName = access["you"].string ?? ""; level = access["level"].string; bypass = access["bypass"] == .bool(true)
            let events = StreamListener(event: { [weak self] line in Task { @MainActor in self?.receive(line, token: token) } }, ended: { [weak self] reason in
                Task { @MainActor in await self?.videoEnded(reason: reason, token: token) }
            })
            let eventSubscription = try await client.events(host: host.name, listener: events)
            guard token == generation else { await client.stop(eventSubscription); return }
            eventID = eventSubscription
            let context = CIContext()
            let relay = PhonePictureRelay { [weak self] picture in
                guard let self, token == self.generation else { return }
                self.image = picture; self.connected = true
            }
            imageRelay = relay
            decoder = H264Decoder(output: { pixels in
                let ci = CIImage(cvPixelBuffer: pixels)
                guard let picture = context.createCGImage(ci, from: ci.extent) else { return }
                relay.push(picture)
            }, failed: { [weak self] reason in
                Task { @MainActor in
                    guard let self, token == self.generation else { return }
                    self.error = reason
                }
            }, requestKeyframe: { [weak self] in
                Task { @MainActor in await self?.requestVideoKeyframe(token: token) }
            })
            let renderer = decoder!
            let video = StreamListener(frame: { [weak self] data in
                switch ScreenHeader(data)?.kind {
                case 1: renderer.push(data)
                case 4:
                    guard let position = PointerPosition(data) else { return }
                    Task { @MainActor in self?.pointerMoved(position, token: token) }
                case 5:
                    guard let shape = PointerShape(data) else { return }
                    Task { @MainActor in
                        guard let self, token == self.generation else { return }
                        self.pointerShapes[shape.id] = shape
                        if let pointer = self.pointer, pointer.shapeID == shape.id { self.pointer = PhonePointer(pointer.position, shape) }
                    }
                default: break
                }
            }, ended: { [weak self] reason in
                Task { @MainActor in await self?.videoEnded(reason: reason, token: token) }
            })
            let view = try await client.video(host: host.name, listener: video)
            guard token == generation else { await client.stop(view.subscription); return }
            videoID = view.subscription
            screen = ScreenGeometry(try JSONValue.parse(view.replyJson)["screen"])
            if keyframePending { await requestVideoKeyframe(token: token) }
            watchRoute(host: host.name, token: token)
        } catch { if token == generation { self.error = error.localizedDescription } }
    }
    /// Re-reads the route while the session lasts: iroh may move a relayed connection to a direct path
    /// (or back) at any time, and the person should know when P2P failed.
    private func watchRoute(host: String, token: Int) {
        routeWatch?.cancel()
        routeWatch = Task { [weak self] in
            while !Task.isCancelled {
                guard let self, token == self.generation else { return }
                if let result = try? await self.client.request(["op": .string("hosts")]),
                   token == self.generation,
                   let entry = result.array.map(PhoneHost.init).first(where: { $0.name == host }) {
                    self.route = entry
                }
                try? await Task.sleep(for: .seconds(5))
            }
        }
    }
    private func pointerMoved(_ position: PointerPosition, token: Int) {
        guard token == generation else { return }
        pointer = PhonePointer(position, pointerShapes[position.shape])
    }
    private func videoEnded(reason: String, token: Int) async {
        guard token == generation, reason != "stopped" else { return }
        generation += 1
        routeWatch?.cancel(); routeWatch = nil; route = nil
        let oldSubscriptions = [videoID, eventID].compactMap { $0 }
        videoID = nil; eventID = nil; screen = nil; keyframePending = false; pointer = nil; pointerShapes = [:]
        imageRelay?.close(); decoder?.close()
        connected = false; error = reason
        // These are leases on the originating core, not IDs that can be reused after reopening.
        for id in oldSubscriptions { await client.stop(id) }
    }
    private func requestVideoKeyframe(token: Int) async {
        guard token == generation, active, !leaving else { return }
        keyframePending = true
        guard let id = videoID else { return }
        keyframePending = false
        await client.keyframe(id)
    }
    private func receive(_ line: String, token: Int) {
        guard token == generation, let event = try? JSONValue.parse(line) else { return }
        switch event["kind"].string {
        case "hello":
            level = event["level"].string; bypass = event["bypass"] == .bool(true); pending = event["pending"].array
            driver = event["driver"]["state"].string ?? "agent"; driverDevice = event["driver"]["device"].string ?? ""
        case "driver": driver = event["state"].string ?? "agent"; driverDevice = event["device"].string ?? ""
        case "decision", "outcome", "task", "note":
            activity.append(event)
            if activity.count > 100 { activity.removeFirst(activity.count - 100) }
            if event["decision"] == .string("ask") { Task { await self.refreshPending() } }
        default: break
        }
    }
    public func setTurn(_ action: String) async {
        guard let selected, !leaving, active else { return }
        let token = generation
        do {
            let result = try await client.turn(host: selected.name, action: action)
            guard token == generation else { return }
            driver = result["driver"]["state"].string ?? driver
            driverDevice = result["driver"]["device"].string ?? ""
        } catch { if token == generation { self.error = error.localizedDescription } }
    }
    public func refreshPending() async {
        guard let selected else { return }
        do {
            let result = try await client.request(["op": .string("channel"), "host": .string(selected.name), "request": .object(["pending": .bool(true)])])
            pending = result["pending"].array
        } catch { self.error = error.localizedDescription }
    }
    public func deny(_ id: String) async {
        guard let selected else { return }
        do {
            _ = try await client.request(["op": .string("channel"), "host": .string(selected.name), "request": .object(["answer": .object(["id": .string(id), "decision": .string("deny")])])])
            await refreshPending()
        } catch { self.error = error.localizedDescription }
    }
    /// Serialize user input; recheck that this phone still owns control when each call starts.
    public func input(tool: String, args: [String: JSONValue], success: (() -> Void)? = nil) {
        guard let selected, ownsControls, screen != nil else { return }
        let prior = inputTail, token = generation
        inputTail = Task { [weak self] in
            await prior?.value
            guard let self, token == self.generation, self.ownsControls else { return }
            do { _ = try await self.client.call(host: selected.name, tool: tool, args: args); success?() }
            catch { self.error = error.localizedDescription }
        }
    }
    public func sendText() {
        guard !text.isEmpty else { return }
        let sent = text
        input(tool: "keyboard", args: ["text": .string(sent)], success: { [weak self] in
            if self?.text == sent { self?.text = "" }
        })
    }
    public func leaveSession() async {
        let handBack = hasDriver
        leaving = true; defer { leaving = false }
        generation += 1; inputTail?.cancel(); inputTail = nil
        routeWatch?.cancel(); routeWatch = nil; route = nil
        for id in [eventID, videoID].compactMap({ $0 }) { await client.stop(id) }
        eventID = nil; videoID = nil; keyframePending = false
        imageRelay?.close(); imageRelay = nil; decoder?.close(); decoder = nil
        if handBack, let selected {
            await withTaskGroup(of: Void.self) { group in
                group.addTask { [client] in _ = try? await client.turn(host: selected.name, action: "hand_back") }
                group.addTask { try? await Task.sleep(for: .seconds(3)) }
                _ = await group.next(); group.cancelAll()
            }
        }
        selected = nil; image = nil; screen = nil; pointer = nil; pointerShapes = [:]; connected = false; pending = []; activity = []; ownName = ""; driverDevice = ""; level = nil; bypass = false; driver = "agent"
    }
    public func suspend() async {
        active = false; lifecycleGeneration += 1
        if let suspending { await suspending.value; return }
        let task = Task { await self.leaveSession(); await self.client.close() }
        suspending = task
        await task.value
        suspending = nil
    }
}

/// The host's pointer as the viewport draws it: where (screen rect units) and what.
public struct PhonePointer: Sendable {
    public let position: PointerPosition
    /// The picture, with its hot spot; `nil`: draw a default arrow.
    public let shape: PointerShape?
    public var visible: Bool { position.visible }
    public var x: Double { position.x }
    public var y: Double { position.y }
    public var shapeID: UInt32 { position.shape }
    init(_ position: PointerPosition, _ shape: PointerShape?) { self.position = position; self.shape = shape }
}

/// One queued MainActor update and one newest picture, even if rendering the UI falls behind.
/// The store still presents its existing native CGImage surface.
private final class PhonePictureRelay: @unchecked Sendable {
    private let lock = NSLock()
    private var latest: CGImage?
    private var scheduled = false, closed = false
    private let show: @MainActor @Sendable (CGImage) -> Void
    init(show: @escaping @MainActor @Sendable (CGImage) -> Void) { self.show = show }
    func push(_ image: CGImage) {
        let enqueue = lock.withLock {
            guard !closed else { return false }
            latest = image
            guard !scheduled else { return false }
            scheduled = true
            return true
        }
        if enqueue { schedule() }
    }
    private func schedule() {
        Task { @MainActor [weak self] in self?.flush() }
    }
    @MainActor private func flush() {
        let picture: CGImage? = lock.withLock {
            defer { latest = nil }
            return closed ? nil : latest
        }
        if let picture { show(picture) }
        let again = lock.withLock {
            if closed || latest == nil { scheduled = false; return false }
            return true
        }
        if again { schedule() }
    }
    func close() { lock.withLock { closed = true; latest = nil } }
}

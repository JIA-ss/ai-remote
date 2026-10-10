import Foundation
import SwiftUI
import CoreImage
import AIRemoteCore

@MainActor @Observable public final class PhoneStore {
    public var hosts: [PhoneHost] = []
    public var selected: PhoneHost?
    public var error: String?
    public var pairingCode = ""
    public var safetyCode: String?
    public var loading = false
    public var pairSheet = false
    public var showActivity = false
    public var connected = false
    public var image: CGImage?
    public var screen: ScreenGeometry?
    public var level: String?
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
    private var generation = 0
    private var inputTail: Task<Void, Never>?
    public var ownsControls: Bool { driver == "person" && driverDevice == ownName && !ownName.isEmpty }
    public var mayControl: Bool { level == "control" || level == "admin" }

    public init(directory: String? = nil) {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        let path = directory ?? base.appendingPathComponent("ai-remote").path
        client = PhoneClient(directory: path)
    }
    public func resume() async {
        guard !loading else { return }
        loading = true; defer { loading = false }
        do {
            try await client.start()
            let status = try await client.request(["op": .string("status")])
            device = status["device"].string ?? ""
            try await refresh()
        } catch { self.error = error.localizedDescription }
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
            ownName = access["you"].string ?? ""; level = access["level"].string
            let events = StreamListener(event: { [weak self] line in Task { @MainActor in self?.receive(line, token: token) } }, ended: { [weak self] reason in
                Task { @MainActor in guard let self, token == self.generation, reason != "stopped" else { return }; self.connected = false; self.error = reason }
            })
            eventID = try await client.events(host: host.name, listener: events)
            let context = CIContext()
            decoder = H264Decoder(output: { [weak self] pixels in
                let ci = CIImage(cvPixelBuffer: pixels)
                guard let picture = context.createCGImage(ci, from: ci.extent) else { return }
                Task { @MainActor in guard let self, token == self.generation else { return }; self.image = picture; self.connected = true }
            }, failed: { [weak self] reason in
                Task { @MainActor in
                    guard let self, token == self.generation else { return }
                    self.error = reason
                    if let id = self.videoID { await self.client.keyframe(id) }
                }
            })
            let renderer = decoder!
            let video = StreamListener(frame: { renderer.push($0) }, ended: { [weak self] reason in
                Task { @MainActor in guard let self, token == self.generation, reason != "stopped" else { return }; self.connected = false; self.error = reason }
            })
            let view = try await client.video(host: host.name, listener: video)
            guard token == generation else { await client.stop(view.subscription); return }
            videoID = view.subscription
            screen = ScreenGeometry(try JSONValue.parse(view.replyJson)["screen"])
        } catch { if token == generation { self.error = error.localizedDescription } }
    }
    private func receive(_ line: String, token: Int) {
        guard token == generation, let event = try? JSONValue.parse(line) else { return }
        switch event["kind"].string {
        case "hello":
            level = event["level"].string; pending = event["pending"].array
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
        guard let selected else { return }
        do {
            let result = try await client.turn(host: selected.name, action: action)
            driver = result["driver"]["state"].string ?? driver
            driverDevice = result["driver"]["device"].string ?? ""
        } catch { self.error = error.localizedDescription }
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
        if ownsControls, let selected {
            await withTaskGroup(of: Void.self) { group in
                group.addTask { [client] in _ = try? await client.turn(host: selected.name, action: "hand_back") }
                group.addTask { try? await Task.sleep(for: .seconds(3)) }
                _ = await group.next(); group.cancelAll()
            }
        }
        generation += 1; inputTail?.cancel(); inputTail = nil
        for id in [eventID, videoID].compactMap({ $0 }) { await client.stop(id) }
        eventID = nil; videoID = nil; decoder?.close(); decoder = nil
        selected = nil; image = nil; screen = nil; connected = false; pending = []; activity = []; ownName = ""; driverDevice = ""
    }
    public func suspend() async { await leaveSession(); await client.close() }
}

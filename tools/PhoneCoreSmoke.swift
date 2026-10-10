import Foundation
import CoreVideo
import AIRemoteCore
import AIRemotePhone

final class Metrics: @unchecked Sendable {
    private let lock = NSLock()
    private var values: [String: JSONValue] = [:]
    func put(_ key: String, _ value: JSONValue) { lock.lock(); defer { lock.unlock() }; values[key] = value }
    func get(_ key: String) -> JSONValue { lock.lock(); defer { lock.unlock() }; return values[key] ?? .null }
    func report() -> String { lock.lock(); defer { lock.unlock() }; return JSONValue.object(values).text }
}

@main struct PhoneCoreSmoke {
    static func main() async throws {
        let args = CommandLine.arguments
        guard args.count == 4 else { fatalError("usage: PhoneCoreSmoke <profile> <private-ticket-file> <report>") }
        let client = PhoneClient(directory: args[1])
        let state = Metrics()
        try await client.start()
        let first = try await client.request(["op": .string("status")])
        let ticket = try String(contentsOfFile: args[2], encoding: .utf8).trimmingCharacters(in: .whitespacesAndNewlines)
        let pair = try await client.request(["op": .string("pair"), "ticket": .string(ticket), "name": .string("phone-smoke")])
        guard pair["level"] == .string("control") else { throw error("pairing did not grant control") }
        state.put("pairing", .string("passed"))
        let hosts = try await client.request(["op": .string("hosts")])
        guard hosts.array.contains(where: { $0["name"] == .string("phone-smoke") }) else { throw error("host list missing paired computer") }
        let turn = try await client.turn(host: "phone-smoke", action: "take_over")
        guard turn["driver"]["state"] == .string("person") else { throw error("take over did not take effect") }
        state.put("take_over", .string("passed"))
        let blocked = try await client.request(["op": .string("call"), "host": .string("phone-smoke"), "tool": .string("mouse"), "args": .object(["action": .string("not-a-valid-action")]), "person": .bool(false)])
        guard blocked["isError"] == .bool(true), blocked["content"].array.first?["text"].string?.contains("took over") == true else { throw error("agent control was not blocked during a person's turn") }
        state.put("agent_blocked", .string("passed"))
        let info = try await client.call(host: "phone-smoke", tool: "system_info", args: [:])
        guard info["structuredContent"]["os"]["family"] == .string("macos") else { throw error("system_info did not return the real Mac") }
        state.put("system_info", .string("passed"))
        let events = StreamListener(event: { text in
            if let v = try? JSONValue.parse(text), v["kind"] == .string("hello") { state.put("events", .string("passed")) }
        })
        let eventID = try await client.events(host: "phone-smoke", listener: events)
        let decoder = H264Decoder(output: { pixels in
            state.put("decoded_width", .number(Double(CVPixelBufferGetWidth(pixels))))
            state.put("decoded_height", .number(Double(CVPixelBufferGetHeight(pixels))))
        }, failed: { state.put("decoder_error", .string($0)) })
        let video = StreamListener(frame: { bytes in
            guard let frame = ScreenFrame(bytes) else { state.put("frame_error", .bool(true)); return }
            state.put("frame_width", .number(Double(frame.width))); decoder.push(bytes)
        })
        let view = try await client.video(host: "phone-smoke", listener: video)
        let geometry = ScreenGeometry(try JSONValue.parse(view.replyJson)["screen"])
        guard geometry?.point(u: 0.5, v: 0.5) != nil, geometry?.point(u: -1, v: 0.5) == nil else { throw error("touch geometry is invalid") }
        state.put("touch_geometry", .string("passed"))
        for _ in 0..<150 {
            if state.get("decoded_width").number != nil && state.get("events") == .string("passed") { break }
            try await Task.sleep(for: .milliseconds(200))
        }
        await client.stop(view.subscription); await client.stop(eventID); decoder.close()
        guard let width = state.get("decoded_width").number, width >= 320, state.get("decoder_error") == .null else { throw error("native VideoToolbox decoding failed: \(state.report())") }
        state.put("native_h264", .string("passed"))
        let handed = try await client.turn(host: "phone-smoke", action: "hand_back")
        guard handed["driver"]["state"] == .string("agent") else { throw error("hand back failed") }
        state.put("hand_back", .string("passed"))
        await client.close(); try await client.start()
        let second = try await client.request(["op": .string("status")])
        guard first["device"] == second["device"] else { throw error("identity changed after reopening") }
        state.put("close_reopen", .string("passed"))
        await client.close()
        state.put("result", .string("passed"))
        try Data(state.report().utf8).write(to: URL(fileURLWithPath: args[3]), options: .atomic)
        print(state.report())
    }
    static func error(_ message: String) -> NSError { NSError(domain: "ai-remote.phone-smoke", code: 1, userInfo: [NSLocalizedDescriptionKey: message]) }
}

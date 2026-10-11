import Foundation
import CoreVideo
import AIRemotePhone

private func require(_ condition: @autoclosure () -> Bool, _ message: String) throws {
    if !condition() { throw NSError(domain: "phone-video", code: 1, userInfo: [NSLocalizedDescriptionKey: message]) }
}

private struct Fixture: Decodable {
    struct Packet: Decodable {
        let packetBase64: String
        let keyframe: Bool
    }
    let width: Int
    let height: Int
    let backend: String
    let frames: [Packet]
}

private final class Probe: @unchecked Sendable {
    private let lock = NSLock()
    let outputReady = DispatchSemaphore(value: 0)
    let recoveryReady = DispatchSemaphore(value: 0)
    let firstEntered = DispatchSemaphore(value: 0)
    let releaseFirst = DispatchSemaphore(value: 0)
    var holdFirst = false
    private var pictures = 0, recoveries = 0
    private var errors: [String] = []
    private var size = (0, 0)
    func picture(_ image: CVPixelBuffer) {
        let hold = lock.withLock {
            pictures += 1
            size = (CVPixelBufferGetWidth(image), CVPixelBufferGetHeight(image))
            return holdFirst && pictures == 1
        }
        if hold {
            firstEntered.signal()
            _ = releaseFirst.wait(timeout: .now() + 10)
        }
        outputReady.signal()
    }
    func recovery() { lock.withLock { recoveries += 1 }; recoveryReady.signal() }
    func failure(_ reason: String) { lock.withLock { errors.append(reason) } }
    var counts: (Int, Int, [String], (Int, Int)) { lock.withLock { (pictures, recoveries, errors, size) } }
}

private final class WeakDecoder: @unchecked Sendable { weak var value: H264Decoder? }

@main struct PhoneDecoderBackpressureSmoke {
    static func main() throws {
        let fixtureData = try Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))
        let jsonDecoder = JSONDecoder()
        jsonDecoder.keyDecodingStrategy = .convertFromSnakeCase
        let fixture = try jsonDecoder.decode(Fixture.self, from: fixtureData)
        let packets = try fixture.frames.map { packet -> Data in
            guard let data = Data(base64Encoded: packet.packetBase64), let frame = ScreenFrame(data),
                  frame.width == fixture.width, frame.height == fixture.height, frame.keyframe == packet.keyframe else {
                throw NSError(domain: "fixture", code: 1)
            }
            return data
        }
        try require(packets.count >= 3 && fixture.frames.first?.keyframe == true && fixture.frames.last?.keyframe == true, "a key/P/recovery sequence is required")
        let key = packets[0], dependent = packets[1], recoveryKey = packets.last!
        try require(!fixture.frames[1].keyframe, "dependency replay requires a real P-frame")

        // Native terminal payload checks on the complete codec-valid GOP.
        let sequential = Probe()
        let decoder = H264Decoder(output: { sequential.picture($0) }, failed: { sequential.failure($0) },
                                  requestKeyframe: { sequential.recovery() })
        for packet in packets {
            decoder.push(packet)
            try require(sequential.outputReady.wait(timeout: .now() + 10) == .success, "sequence did not decode")
        }
        let sequentialStats = decoder.diagnostics
        decoder.close()
        try require(sequential.counts.0 == packets.count && sequential.counts.2.isEmpty, "sequence failed or lost pictures")
        try require(sequential.counts.3 == (fixture.width, fixture.height), "decoded dimensions mismatch")

        // Hold the current output deterministically while transport bursts 2000 dependent packets.
        // The waiting limit must stay at two; a discarded chain must request one recovery IDR.
        let burst = Probe()
        burst.holdFirst = true
        let overloaded = H264Decoder(output: { burst.picture($0) }, failed: { burst.failure($0) },
                                     requestKeyframe: { burst.recovery() })
        overloaded.push(key)
        try require(burst.firstEntered.wait(timeout: .now() + 10) == .success, "first output did not enter")
        for _ in 0..<2000 { overloaded.push(dependent) }
        let peak = overloaded.diagnostics
        try require(peak.maximumWaitingFrames == 2 && peak.waitingFrames <= 2 && peak.droppedFrames == 2000, "compressed backlog grew or a broken dependency chain remained")
        burst.releaseFirst.signal()
        try require(burst.outputReady.wait(timeout: .now() + 10) == .success, "held output did not finish")
        try require(burst.recoveryReady.wait(timeout: .now() + 10) == .success, "overflow did not request a keyframe")
        overloaded.push(recoveryKey)
        try require(burst.outputReady.wait(timeout: .now() + 10) == .success, "recovery IDR did not decode")
        let burstStats = overloaded.diagnostics
        overloaded.close()
        try require(burst.counts.0 == 2 && burst.counts.1 == 1 && burst.counts.2.isEmpty, "overflow recovery was not coalesced or clean")

        // Starting mid-GOP must wait for a keyframe without decoding missing-reference P frames.
        let late = Probe()
        let joining = H264Decoder(output: { late.picture($0) }, failed: { late.failure($0) },
                                  requestKeyframe: { late.recovery() })
        joining.push(dependent)
        try require(late.recoveryReady.wait(timeout: .now() + 10) == .success, "mid-GOP join did not request recovery")
        try require(late.counts.0 == 0 && late.counts.2.isEmpty, "a missing-reference frame was presented")
        joining.push(recoveryKey)
        try require(late.outputReady.wait(timeout: .now() + 10) == .success, "mid-GOP recovery failed")
        joining.close()

        // Retire while output is busy. Close clears pending pictures before waiting for the worker.
        let retired = Probe()
        retired.holdFirst = true
        let retiring = H264Decoder(output: { retired.picture($0) }, failed: { retired.failure($0) },
                                   requestKeyframe: { retired.recovery() })
        retiring.push(key)
        try require(retired.firstEntered.wait(timeout: .now() + 10) == .success, "retirement output did not enter")
        retiring.push(dependent)
        let closeStarted = DispatchSemaphore(value: 0), closeFinished = DispatchSemaphore(value: 0)
        DispatchQueue.global().async { closeStarted.signal(); retiring.close(); closeFinished.signal() }
        try require(closeStarted.wait(timeout: .now() + 2) == .success, "close did not start")
        let deadline = Date().addingTimeInterval(2)
        while retiring.diagnostics.waitingFrames != 0 && Date() < deadline { Thread.sleep(forTimeInterval: 0.001) }
        try require(retiring.diagnostics.waitingFrames == 0, "retirement did not clear pending work")
        retired.releaseFirst.signal()
        try require(closeFinished.wait(timeout: .now() + 5) == .success, "close did not finish")
        let before = retired.counts
        retiring.push(recoveryKey)
        retiring.push(Data([0]))
        retiring.close()
        try require(retired.counts.0 == before.0 && retired.counts.1 == before.1 && retired.counts.2 == before.2, "retired callbacks or input produced output")

        // A client may close from its own output callback; this must not dispatch-sync deadlock.
        let weak = WeakDecoder()
        let reentrantDone = DispatchSemaphore(value: 0)
        let reentrant = H264Decoder(output: { _ in weak.value?.close(); reentrantDone.signal() },
                                    failed: { _ in })
        weak.value = reentrant
        reentrant.push(key)
        try require(reentrantDone.wait(timeout: .now() + 10) == .success, "closing from output deadlocked")
        reentrant.close()

        let report: [String: Any] = [
            "result": "passed", "source": "synthetic codec-valid H.264 replay; no personal capture",
            "fixture_backend": fixture.backend, "width": fixture.width, "height": fixture.height,
            "sequential_frames": sequential.counts.0, "burst_submitted": 2000,
            "maximum_waiting_frames": burstStats.maximumWaitingFrames, "dropped_dependent_frames": burstStats.droppedFrames,
            "recovery_requests": burst.counts.1, "recovery_decoded": true, "mid_gop_recovery": true,
            "retired_callbacks": "ignored", "reentrant_close": "passed",
            "hardware_decoder": sequentialStats.hardwareDecoder.map { $0 as Any } ?? NSNull(),
            "limitation": "macOS native decoding only; not iPhone capture, display, performance or E2E acceptance"
        ]
        print(String(data: try JSONSerialization.data(withJSONObject: report, options: [.sortedKeys]), encoding: .utf8)!)
    }
}

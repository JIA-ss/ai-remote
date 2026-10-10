import Foundation
import CoreVideo
import AIRemotePhone

final class DecodeResult: @unchecked Sendable {
    let done = DispatchSemaphore(value: 0)
    let lock = NSLock()
    var width = 0, height = 0
    var error: String?
    func picture(_ pixels: CVPixelBuffer) {
        lock.lock(); width = CVPixelBufferGetWidth(pixels); height = CVPixelBufferGetHeight(pixels); lock.unlock()
        done.signal()
    }
    func fail(_ message: String) { lock.lock(); error = message; lock.unlock(); done.signal() }
}

@main struct PhoneDecoderSmoke {
    static func main() throws {
        let data = try Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))
        guard let frame = ScreenFrame(data) else { throw NSError(domain: "frame", code: 1) }
        assert(ScreenFrame(Data(data.dropLast())) == nil, "truncated frames must be refused")
        let result = DecodeResult()
        let decoder = H264Decoder(output: { result.picture($0) }, failed: { result.fail($0) })
        decoder.push(data)
        let ready = result.done.wait(timeout: .now() + 10) == .success
        decoder.close()
        guard ready, result.error == nil, result.width == frame.width, result.height == frame.height else {
            throw NSError(domain: "decode", code: 1, userInfo: [NSLocalizedDescriptionKey: result.error ?? "no matching decoded frame"])
        }
        let v: [String: Any] = ["source": "replay of a previously captured real Mac frame", "result": "passed", "width": result.width, "height": result.height, "truncated_frame": "refused"]
        print(String(data: try JSONSerialization.data(withJSONObject: v), encoding: .utf8)!)
    }
}

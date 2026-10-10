import Foundation
import CoreMedia
import CoreVideo
import VideoToolbox

/// Native hardware decoding. Transport remains screen/1; only Annex B to AVCC conversion is local.
public final class H264Decoder: @unchecked Sendable {
    private let queue = DispatchQueue(label: "ai-remote.video.decode")
    private var session: VTDecompressionSession?
    private var format: CMVideoFormatDescription?
    private var sps = Data(), pps = Data()
    private let output: @Sendable (CVPixelBuffer) -> Void
    private let failed: @Sendable (String) -> Void
    public init(output: @escaping @Sendable (CVPixelBuffer) -> Void, failed: @escaping @Sendable (String) -> Void) {
        self.output = output; self.failed = failed
    }
    public func push(_ bytes: Data) {
        guard let frame = ScreenFrame(bytes) else { failed("无效的视频帧"); return }
        queue.async { [self] in
            do { try decode(frame) } catch { failed(error.localizedDescription) }
        }
    }
    public func close() {
        queue.sync { self.reset() }
    }
    private func reset() {
        if let session { VTDecompressionSessionWaitForAsynchronousFrames(session); VTDecompressionSessionInvalidate(session) }
        session = nil; format = nil
    }
    private func check(_ status: OSStatus, _ operation: String) throws {
        if status != noErr { throw NSError(domain: "ai-remote.video", code: Int(status), userInfo: [NSLocalizedDescriptionKey: "\(operation)失败（\(status)）"]) }
    }
    private func configure() throws {
        reset()
        try sps.withUnsafeBytes { a in
            try pps.withUnsafeBytes { b in
                let pointers = [a.bindMemory(to: UInt8.self).baseAddress!, b.bindMemory(to: UInt8.self).baseAddress!]
                let sizes = [sps.count, pps.count]
                try check(CMVideoFormatDescriptionCreateFromH264ParameterSets(allocator: kCFAllocatorDefault,
                    parameterSetCount: 2, parameterSetPointers: pointers, parameterSetSizes: sizes,
                    nalUnitHeaderLength: 4, formatDescriptionOut: &format), "创建视频格式")
            }
        }
        var callback = VTDecompressionOutputCallbackRecord(decompressionOutputCallback: { ref, _, status, _, image, _, _ in
            guard let ref else { return }
            let decoder = Unmanaged<H264Decoder>.fromOpaque(ref).takeUnretainedValue()
            if status == noErr, let image { decoder.output(image) }
            else if status != noErr { decoder.failed("视频解码失败（\(status)）") }
        }, decompressionOutputRefCon: Unmanaged.passUnretained(self).toOpaque())
        let attributes = [kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA,
                          kCVPixelBufferIOSurfacePropertiesKey: [:]] as CFDictionary
        try check(VTDecompressionSessionCreate(allocator: kCFAllocatorDefault, formatDescription: format!,
            decoderSpecification: nil, imageBufferAttributes: attributes,
            outputCallback: &callback, decompressionSessionOut: &session), "启动视频解码器")
    }
    private func decode(_ frame: ScreenFrame) throws {
        let nals = frame.nalUnits
        let nextSPS = nals.first(where: { $0.first.map { $0 & 31 == 7 } ?? false })
        let nextPPS = nals.first(where: { $0.first.map { $0 & 31 == 8 } ?? false })
        if frame.keyframe, let nextSPS, let nextPPS, session == nil || nextSPS != sps || nextPPS != pps {
            sps = nextSPS; pps = nextPPS
            try configure()
        }
        guard let session, let format else { return }
        var avcc = Data()
        for nal in nals {
            var length = UInt32(nal.count).bigEndian
            withUnsafeBytes(of: &length) { avcc.append(contentsOf: $0) }
            avcc.append(nal)
        }
        guard !avcc.isEmpty else { return }
        var block: CMBlockBuffer?
        try check(CMBlockBufferCreateWithMemoryBlock(allocator: kCFAllocatorDefault, memoryBlock: nil,
            blockLength: avcc.count, blockAllocator: kCFAllocatorDefault, customBlockSource: nil,
            offsetToData: 0, dataLength: avcc.count, flags: 0, blockBufferOut: &block), "创建视频缓冲区")
        try avcc.withUnsafeBytes { bytes in
            try check(CMBlockBufferReplaceDataBytes(with: bytes.baseAddress!, blockBuffer: block!,
                offsetIntoDestination: 0, dataLength: avcc.count), "复制视频帧")
        }
        var sample: CMSampleBuffer?
        var timing = CMSampleTimingInfo(duration: .invalid,
            presentationTimeStamp: CMTime(value: Int64(frame.microseconds), timescale: 1_000_000), decodeTimeStamp: .invalid)
        var size = avcc.count
        try check(CMSampleBufferCreateReady(allocator: kCFAllocatorDefault, dataBuffer: block,
            formatDescription: format, sampleCount: 1, sampleTimingEntryCount: 1,
            sampleTimingArray: &timing, sampleSizeEntryCount: 1, sampleSizeArray: &size,
            sampleBufferOut: &sample), "准备视频帧")
        try check(VTDecompressionSessionDecodeFrame(session, sampleBuffer: sample!,
            flags: [._EnableAsynchronousDecompression], frameRefcon: nil, infoFlagsOut: nil), "解码视频帧")
    }
    deinit { reset() }
}

import Foundation
import CoreMedia
import CoreVideo
import VideoToolbox

public struct H264DecoderDiagnostics: Sendable {
    public let waitingFrames: Int
    public let maximumWaitingFrames: Int
    public let droppedFrames: Int
    public let decodedFrames: Int
    public let keyframeRequests: Int
    public let hardwareDecoder: Bool?
}

/// Native decoding (H.264 or HEVC, whichever the frame header says; the codec may change at a
/// keyframe) with two waiting compressed frames and one frame currently being decoded. Losing
/// dependent frames discards the chain and requests a fresh keyframe.
public final class H264Decoder: @unchecked Sendable {
    private let queue = DispatchQueue(label: "ai-remote.video.decode", qos: .userInteractive)
    private let queueKey = DispatchSpecificKey<Bool>()
    private let lock = NSLock()
    // Only this small ingress state is accessed from the transport/native callback threads.
    private var waiting: [ScreenFrame] = []
    private var draining = false, closed = false, awaitingKeyframe = true, recoveryNotified = false
    private var recoveryPending = false
    private var recoveryReason: String?
    private var epoch: UInt64 = 0
    private var decodedPicture: CVPixelBuffer?
    private var decodeError: String?
    private var peakWaiting = 0, dropped = 0, decoded = 0, keyframesRequested = 0
    private var hardwareDecoder: Bool?
    // The native session and its retained callback context belong to the serial decode worker.
    private var session: VTDecompressionSession?
    private var format: CMVideoFormatDescription?
    private var callbackContext: CallbackContext?
    private var vps = Data(), sps = Data(), pps = Data()
    private var codec: ScreenCodec?
    private let output: @Sendable (CVPixelBuffer) -> Void
    private let failed: @Sendable (String) -> Void
    private let requestKeyframe: @Sendable () -> Void

    private final class CallbackContext {
        weak var decoder: H264Decoder?
        let epoch: UInt64
        init(decoder: H264Decoder, epoch: UInt64) { self.decoder = decoder; self.epoch = epoch }
    }
    private enum Work {
        case frame(ScreenFrame)
        case recovery(String?)
    }

    public init(output: @escaping @Sendable (CVPixelBuffer) -> Void,
                failed: @escaping @Sendable (String) -> Void,
                requestKeyframe: @escaping @Sendable () -> Void = {}) {
        self.output = output; self.failed = failed; self.requestKeyframe = requestKeyframe
        queue.setSpecific(key: queueKey, value: true)
    }

    public var diagnostics: H264DecoderDiagnostics {
        lock.withLock {
            H264DecoderDiagnostics(waitingFrames: waiting.count, maximumWaitingFrames: peakWaiting,
                droppedFrames: dropped, decodedFrames: decoded, keyframeRequests: keyframesRequested,
                hardwareDecoder: hardwareDecoder)
        }
    }

    public func push(_ bytes: Data) {
        guard !lock.withLock({ closed }) else { return }
        guard let frame = ScreenFrame(bytes) else { recover("无效的视频帧"); return }
        guard frame.width <= 4096, frame.height <= 4096 else { recover("视频尺寸超出支持范围"); return }
        var startDrain = false, request = false
        lock.lock()
        if closed { lock.unlock(); return }
        if awaitingKeyframe && !frame.keyframe {
            dropped += 1
            if !recoveryNotified {
                recoveryNotified = true; request = true
            }
        } else {
            if waiting.count == 2 {
                // Never drop just one P-frame and then decode its dependants.
                dropped += waiting.count
                waiting.removeAll(keepingCapacity: true)
                awaitingKeyframe = true
            }
            if frame.keyframe {
                awaitingKeyframe = false; recoveryNotified = false
            }
            if awaitingKeyframe {
                dropped += 1
                if !recoveryNotified {
                    recoveryNotified = true; request = true
                }
            } else {
                waiting.append(frame)
                peakWaiting = max(peakWaiting, waiting.count)
                if !draining { draining = true; startDrain = true }
            }
        }
        if request {
            recoveryPending = true
            if !draining { draining = true; startDrain = true }
        }
        lock.unlock()
        // At most one drain block exists, regardless of how fast network frames arrive.
        if startDrain { queue.async { [weak self] in self?.drain() } }
    }

    public func close() {
        lock.withLock {
            closed = true; epoch &+= 1
            waiting.removeAll(); decodedPicture = nil; decodeError = nil
            recoveryPending = false; recoveryReason = nil
        }
        // Output callbacks run on this worker after native decode completes; closing from an
        // output callback must not synchronously dispatch back to the same queue.
        if DispatchQueue.getSpecific(key: queueKey) == true { reset() }
        else { queue.sync { self.reset() } }
    }

    private func drain() {
        while true {
            let work: Work? = lock.withLock {
                guard !closed else { draining = false; return nil }
                if recoveryPending {
                    recoveryPending = false
                    let reason = recoveryReason; recoveryReason = nil
                    keyframesRequested += 1
                    return .recovery(reason)
                }
                guard !waiting.isEmpty else { draining = false; return nil }
                return .frame(waiting.removeFirst())
            }
            guard let work else { return }
            if case .recovery(let reason) = work {
                guard !lock.withLock({ closed }) else { continue }
                if let reason { failed(reason) }
                if !lock.withLock({ closed }) { requestKeyframe() }
                continue
            }
            guard case .frame(let frame) = work else { continue }
            do {
                try decode(frame)
                let result: (CVPixelBuffer?, String?) = lock.withLock {
                    defer { decodedPicture = nil; decodeError = nil }
                    guard !closed else { return (nil, nil) }
                    if decodedPicture != nil { decoded += 1 }
                    return (decodedPicture, decodeError)
                }
                if let error = result.1 { throw failure(error) }
                if let picture = result.0 {
                    guard CVPixelBufferGetWidth(picture) == frame.width, CVPixelBufferGetHeight(picture) == frame.height else {
                        throw failure("视频尺寸与帧信息不一致")
                    }
                    output(picture)
                }
            } catch {
                reset()
                recover(error.localizedDescription)
            }
        }
    }

    private func recover(_ reason: String?) {
        let startDrain: Bool = lock.withLock {
            guard !closed else { return false }
            dropped += waiting.count
            waiting.removeAll(keepingCapacity: true)
            awaitingKeyframe = true
            if !recoveryNotified {
                recoveryNotified = true; recoveryPending = true; recoveryReason = reason
            } else if recoveryPending, recoveryReason == nil { recoveryReason = reason }
            guard recoveryPending, !draining else { return false }
            draining = true
            return true
        }
        if startDrain { queue.async { [weak self] in self?.drain() } }
    }

    private func reset() {
        lock.withLock { epoch &+= 1; decodedPicture = nil; decodeError = nil }
        if let session {
            VTDecompressionSessionWaitForAsynchronousFrames(session)
            VTDecompressionSessionInvalidate(session)
        }
        session = nil; format = nil; callbackContext = nil; codec = nil
    }

    private func receive(epoch callbackEpoch: UInt64, status: OSStatus, image: CVPixelBuffer?) {
        lock.withLock {
            guard !closed, callbackEpoch == epoch else { return }
            if status == noErr, let image { decodedPicture = image }
            else { decodeError = status == noErr ? "视频解码器未输出画面" : "视频解码失败（\(status)）" }
        }
    }

    private func failure(_ message: String, status: OSStatus = -1) -> NSError {
        NSError(domain: "ai-remote.video", code: Int(status), userInfo: [NSLocalizedDescriptionKey: message])
    }

    private func check(_ status: OSStatus, _ operation: String) throws {
        if status != noErr { throw failure("\(operation)失败（\(status)）", status: status) }
    }

    private func configure(width: Int, height: Int, codec: ScreenCodec) throws {
        reset()
        self.codec = codec
        switch codec {
        case .h264:
            try sps.withUnsafeBytes { a in
                try pps.withUnsafeBytes { b in
                    let pointers = [a.bindMemory(to: UInt8.self).baseAddress!, b.bindMemory(to: UInt8.self).baseAddress!]
                    let sizes = [sps.count, pps.count]
                    try check(CMVideoFormatDescriptionCreateFromH264ParameterSets(allocator: kCFAllocatorDefault,
                        parameterSetCount: 2, parameterSetPointers: pointers, parameterSetSizes: sizes,
                        nalUnitHeaderLength: 4, formatDescriptionOut: &format), "创建视频格式")
                }
            }
        case .hevc:
            try vps.withUnsafeBytes { v in
                try sps.withUnsafeBytes { a in
                    try pps.withUnsafeBytes { b in
                        let pointers = [v.bindMemory(to: UInt8.self).baseAddress!, a.bindMemory(to: UInt8.self).baseAddress!,
                                        b.bindMemory(to: UInt8.self).baseAddress!]
                        let sizes = [vps.count, sps.count, pps.count]
                        try check(CMVideoFormatDescriptionCreateFromHEVCParameterSets(allocator: kCFAllocatorDefault,
                            parameterSetCount: 3, parameterSetPointers: pointers, parameterSetSizes: sizes,
                            nalUnitHeaderLength: 4, extensions: nil, formatDescriptionOut: &format), "创建视频格式")
                    }
                }
            }
        case .av1:
            throw failure("此设备不支持 AV1 视频")
        }
        guard let format else { throw failure("缺少视频格式") }
        let dimensions = CMVideoFormatDescriptionGetDimensions(format)
        // Validate SPS dimensions before allocating a decoder's image buffers, not after decode.
        guard dimensions.width == width, dimensions.height == height else { throw failure("视频格式尺寸与帧信息不一致") }
        let context = CallbackContext(decoder: self, epoch: lock.withLock { epoch })
        callbackContext = context
        var callback = VTDecompressionOutputCallbackRecord(decompressionOutputCallback: { ref, _, status, _, image, _, _ in
            guard let ref else { return }
            let context = Unmanaged<CallbackContext>.fromOpaque(ref).takeUnretainedValue()
            context.decoder?.receive(epoch: context.epoch, status: status, image: image)
        }, decompressionOutputRefCon: Unmanaged.passUnretained(context).toOpaque())
        let attributes = [kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA,
                          kCVPixelBufferIOSurfacePropertiesKey: [:]] as CFDictionary
        let specification = [kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder: true] as CFDictionary
        try check(VTDecompressionSessionCreate(allocator: kCFAllocatorDefault, formatDescription: format,
            decoderSpecification: specification, imageBufferAttributes: attributes,
            outputCallback: &callback, decompressionSessionOut: &session), "启动视频解码器")
        if let session {
            // This preference may be unsupported by a decoder; decoding remains usable in that case.
            _ = VTSessionSetProperty(session, key: kVTDecompressionPropertyKey_RealTime, value: kCFBooleanTrue)
            var value: Unmanaged<CFTypeRef>?
            let status = VTSessionCopyProperty(session, key: kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder,
                allocator: kCFAllocatorDefault, valueOut: &value)
            let queried = value?.takeRetainedValue()
            lock.withLock { hardwareDecoder = status == noErr ? queried as? Bool : nil }
        }
    }

    private func decode(_ frame: ScreenFrame) throws {
        guard !lock.withLock({ closed }) else { return }
        let nals = frame.nalUnits
        guard !nals.isEmpty else { throw failure("无效的视频内容") }
        // H.264: the NAL type is the low 5 bits; HEVC: bits 1-6 of the first header byte.
        let type: (Data) -> UInt8 = { nal in
            guard let first = nal.first else { return 0xff }
            return frame.codec == .hevc ? (first >> 1) & 0x3f : first & 31
        }
        let (vpsType, spsType, ppsType): (UInt8, UInt8, UInt8) = frame.codec == .hevc ? (32, 33, 34) : (0xff, 7, 8)
        let picture: (UInt8) -> Bool = frame.codec == .hevc ? { (16...21).contains($0) } : { $0 == 5 }
        let nextVPS = nals.first(where: { type($0) == vpsType })
        let nextSPS = nals.first(where: { type($0) == spsType })
        let nextPPS = nals.first(where: { type($0) == ppsType })
        if frame.keyframe {
            guard nals.contains(where: { picture(type($0)) }), let nextSPS, let nextPPS,
                  frame.codec != .hevc || nextVPS != nil else { throw failure("关键帧缺少图像或参数") }
            let nextVPS = nextVPS ?? Data()
            if session == nil || codec != frame.codec || nextVPS != vps || nextSPS != sps || nextPPS != pps {
                vps = nextVPS; sps = nextSPS; pps = nextPPS
                try configure(width: frame.width, height: frame.height, codec: frame.codec)
            }
        }
        guard let session, let format, codec == frame.codec else { throw failure("等待可解码的关键帧") }
        var avcc = Data()
        avcc.reserveCapacity(frame.payload.count + nals.count * 4)
        for nal in nals {
            var length = UInt32(nal.count).bigEndian
            withUnsafeBytes(of: &length) { avcc.append(contentsOf: $0) }
            avcc.append(nal)
        }
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
            presentationTimeStamp: CMTime(value: Int64(clamping: frame.microseconds), timescale: 1_000_000), decodeTimeStamp: .invalid)
        var size = avcc.count
        try check(CMSampleBufferCreateReady(allocator: kCFAllocatorDefault, dataBuffer: block,
            formatDescription: format, sampleCount: 1, sampleTimingEntryCount: 1,
            sampleTimingArray: &timing, sampleSizeEntryCount: 1, sampleSizeArray: &size,
            sampleBufferOut: &sample), "准备视频帧")
        guard !lock.withLock({ closed }) else { return }
        // With both asynchronous flags clear, Apple's callback completes before this returns.
        // This also bounds VideoToolbox's internal work to the current frame, not an implicit queue.
        try check(VTDecompressionSessionDecodeFrame(session, sampleBuffer: sample!,
            flags: [], frameRefcon: nil, infoFlagsOut: nil), "解码视频帧")
    }

    deinit { reset() }
}

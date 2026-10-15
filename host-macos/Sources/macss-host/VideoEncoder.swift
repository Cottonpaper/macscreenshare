import Foundation
import CoreMedia
import CoreVideo
import VideoToolbox

protocol VideoEncoderDelegate: AnyObject {
    func encoder(_ encoder: VideoEncoder,
                 didEncodeFrame data: Data,
                 keyframe: Bool,
                 ptsMs: UInt32,
                 encodeMs: Float)
    func encoder(_ encoder: VideoEncoder, didFail error: Error?)
}

/// Low-latency hardware H.264/HEVC encoder that emits Annex-B access units.
final class VideoEncoder {
    weak var delegate: VideoEncoderDelegate?

    private let session: VTCompressionSession
    private let codec: Codec
    private let fps: Int
    private var forceKeyframeNext = true

    init(width: Int, height: Int, fps: Int, bitrateKbps: Int, codec: Codec) throws {
        self.codec = codec
        self.fps = max(1, fps)

        var session: VTCompressionSession?
        let codecType: CMVideoCodecType = (codec == .hevc)
            ? kCMVideoCodecType_HEVC
            : kCMVideoCodecType_H264

        // Low-latency rate control is an encoder *specification* (used to select
        // and configure the encoder), not a compression property.
        let encoderSpecification: [CFString: Any] = [
            kVTVideoEncoderSpecification_EnableLowLatencyRateControl: kCFBooleanTrue as Any
        ]

        let status = VTCompressionSessionCreate(
            allocator: kCFAllocatorDefault,
            width: Int32(width),
            height: Int32(height),
            codecType: codecType,
            encoderSpecification: encoderSpecification as CFDictionary,
            imageBufferAttributes: nil,
            compressedDataAllocator: nil,
            outputCallback: nil,
            refcon: nil,
            compressionSessionOut: &session
        )
        guard status == noErr, let created = session else {
            throw HostError.encoderCreate(status)
        }
        self.session = created

        setProperty(kVTCompressionPropertyKey_RealTime, kCFBooleanTrue)
        setProperty(kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanFalse)
        setProperty(kVTCompressionPropertyKey_ExpectedFrameRate, NSNumber(value: self.fps))
        setProperty(kVTCompressionPropertyKey_MaxKeyFrameInterval, NSNumber(value: self.fps * 2))
        setProperty(kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, NSNumber(value: 2.0))
        setProperty(kVTCompressionPropertyKey_AverageBitRate,
                    NSNumber(value: max(500, bitrateKbps) * 1000))
        if codec == .hevc {
            setProperty(kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_HEVC_Main_AutoLevel)
        } else {
            setProperty(kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_H264_High_AutoLevel)
        }

        VTCompressionSessionPrepareToEncodeFrames(created)
    }

    private func setProperty(_ key: CFString, _ value: CFTypeRef?) {
        let status = VTSessionSetProperty(session, key: key, value: value)
        if status != noErr {
            Log.warn("encoder: failed to set \(key) (status \(status))")
        }
    }

    /// Ask the encoder to emit an IDR on the next frame.
    func requestKeyframe() {
        forceKeyframeNext = true
    }

    func encode(pixelBuffer: CVPixelBuffer, pts: CMTime) {
        var frameProperties: CFDictionary?
        if forceKeyframeNext {
            frameProperties = [kVTEncodeFrameOptionKey_ForceKeyFrame: true] as CFDictionary
            forceKeyframeNext = false
        }

        let started = CFAbsoluteTimeGetCurrent()
        let duration = CMTime(value: 1, timescale: CMTimeScale(fps))

        let status = VTCompressionSessionEncodeFrame(
            session,
            imageBuffer: pixelBuffer,
            presentationTimeStamp: pts,
            duration: duration,
            frameProperties: frameProperties,
            infoFlagsOut: nil
        ) { [weak self] status, _, sampleBuffer in
            guard let self else { return }
            let encodeMs = Float((CFAbsoluteTimeGetCurrent() - started) * 1000)
            if status != noErr {
                self.delegate?.encoder(self, didFail: nil)
                return
            }
            guard let sb = sampleBuffer, CMSampleBufferDataIsReady(sb) else { return }
            let keyframe = Self.isKeyframe(sb)
            guard let annexB = Self.annexB(from: sb, keyframe: keyframe) else { return }
            let ptsMs = UInt32(max(0, CMTimeGetSeconds(pts) * 1000))
            self.delegate?.encoder(self, didEncodeFrame: annexB,
                                   keyframe: keyframe, ptsMs: ptsMs, encodeMs: encodeMs)
        }

        if status != noErr {
            delegate?.encoder(self, didFail: nil)
        }
    }

    func invalidate() {
        VTCompressionSessionInvalidate(session)
    }

    // MARK: - Sample buffer conversion

    private static func isKeyframe(_ sb: CMSampleBuffer) -> Bool {
        guard let attachments = CMSampleBufferGetSampleAttachmentsArray(
            sb, createIfNecessary: false) as? [[CFString: Any]],
            let first = attachments.first,
            let notSync = first[kCMSampleAttachmentKey_NotSync] as? Bool
        else {
            return true // no attachment means sync
        }
        return !notSync
    }

    private static func annexB(from sb: CMSampleBuffer, keyframe: Bool) -> Data? {
        guard let format = CMSampleBufferGetFormatDescription(sb) else { return nil }
        let startCode: [UInt8] = [0x00, 0x00, 0x00, 0x01]
        var out = Data()

        if keyframe {
            for set in parameterSets(from: format) {
                out.append(contentsOf: startCode)
                out.append(set)
            }
        }

        guard let block = CMSampleBufferGetDataBuffer(sb) else { return nil }
        var lengthAtOffset = 0
        var totalLength = 0
        var dataPointer: UnsafeMutablePointer<Int8>?
        let st = CMBlockBufferGetDataPointer(
            block, atOffset: 0,
            lengthAtOffsetOut: &lengthAtOffset,
            totalLengthOut: &totalLength,
            dataPointerOut: &dataPointer)
        guard st == kCMBlockBufferNoErr, let base = dataPointer, totalLength > 0 else { return nil }

        let nalLengthSize = Self.nalLengthSize(from: format)
        var offset = 0
        while offset + nalLengthSize <= totalLength {
            var nalLength = 0
            for i in 0..<nalLengthSize {
                nalLength = (nalLength << 8) | Int(UInt8(bitPattern: base[offset + i]))
            }
            offset += nalLengthSize
            guard nalLength > 0, offset + nalLength <= totalLength else { break }
            out.append(contentsOf: startCode)
            base.advanced(by: offset)
                .withMemoryRebound(to: UInt8.self, capacity: nalLength) { ptr in
                    out.append(ptr, count: nalLength)
                }
            offset += nalLength
        }
        return out
    }

    private static func nalLengthSize(from format: CMFormatDescription) -> Int {
        let subtype = CMFormatDescriptionGetMediaSubType(format)
        var length: Int32 = 0
        let status: OSStatus
        if subtype == kCMVideoCodecType_HEVC {
            status = CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                format, parameterSetIndex: 0, parameterSetPointerOut: nil,
                parameterSetSizeOut: nil, parameterSetCountOut: nil,
                nalUnitHeaderLengthOut: &length)
        } else {
            status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                format, parameterSetIndex: 0, parameterSetPointerOut: nil,
                parameterSetSizeOut: nil, parameterSetCountOut: nil,
                nalUnitHeaderLengthOut: &length)
        }
        return (status == noErr && length > 0) ? Int(length) : 4
    }

    private static func parameterSets(from format: CMFormatDescription) -> [Data] {
        let subtype = CMFormatDescriptionGetMediaSubType(format)
        var count = 0
        let isHEVC = (subtype == kCMVideoCodecType_HEVC)

        let probe: OSStatus
        if isHEVC {
            probe = CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                format, parameterSetIndex: 0, parameterSetPointerOut: nil,
                parameterSetSizeOut: nil, parameterSetCountOut: &count,
                nalUnitHeaderLengthOut: nil)
        } else {
            probe = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                format, parameterSetIndex: 0, parameterSetPointerOut: nil,
                parameterSetSizeOut: nil, parameterSetCountOut: &count,
                nalUnitHeaderLengthOut: nil)
        }
        guard probe == noErr, count > 0 else { return [] }

        var sets: [Data] = []
        for index in 0..<count {
            var pointer: UnsafePointer<UInt8>?
            var size = 0
            let status: OSStatus
            if isHEVC {
                status = CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                    format, parameterSetIndex: index, parameterSetPointerOut: &pointer,
                    parameterSetSizeOut: &size, parameterSetCountOut: nil,
                    nalUnitHeaderLengthOut: nil)
            } else {
                status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                    format, parameterSetIndex: index, parameterSetPointerOut: &pointer,
                    parameterSetSizeOut: &size, parameterSetCountOut: nil,
                    nalUnitHeaderLengthOut: nil)
            }
            if status == noErr, let pointer, size > 0 {
                sets.append(Data(bytes: pointer, count: size))
            }
        }
        return sets
    }
}

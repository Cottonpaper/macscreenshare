import AppKit
import Foundation
import AVFoundation
import CoreGraphics
import CoreMedia
import CoreVideo
import ScreenCaptureKit

struct DisplayInfo {
    let displayID: CGDirectDisplayID
    let widthPixels: Int
    let heightPixels: Int
    let scale: Float
}

protocol CaptureEngineDelegate: AnyObject {
    func captureEngine(_ engine: CaptureEngine, didOutput pixelBuffer: CVPixelBuffer, pts: CMTime)
    func captureEngine(_ engine: CaptureEngine, didStopWith error: Error?)
}

/// Wraps an `SCStream` configured for low-latency screen capture.
final class CaptureEngine: NSObject, SCStreamOutput, SCStreamDelegate {
    weak var delegate: CaptureEngineDelegate?

    let info: DisplayInfo
    // `SCStream` takes its delegate at initialization and exposes no setter, so
    // the engine is created first and the stream is attached afterwards.
    private var stream: SCStream!
    private let sampleQueue: DispatchQueue
    private let frameStatusKey = SCStreamFrameInfo.status

    private init(info: DisplayInfo, queue: DispatchQueue) {
        self.info = info
        self.sampleQueue = queue
    }

    /// Build a capture engine for the main display (falling back to any display).
    static func mainDisplay(fps: Int, captureCursor: Bool = true) async throws -> CaptureEngine {
        let content = try await SCShareableContent.excludingDesktopWindows(
            false, onScreenWindowsOnly: true)

        let mainID = CGMainDisplayID()
        guard let display = content.displays.first(where: { $0.displayID == mainID })
            ?? content.displays.first
        else {
            throw HostError.noDisplay
        }

        let scale = Float(NSScreen.main?.backingScaleFactor ?? 1)
        let info = DisplayInfo(
            displayID: display.displayID,
            widthPixels: display.width,
            heightPixels: display.height,
            scale: scale
        )

        let config = SCStreamConfiguration()
        config.width = display.width
        config.height = display.height
        config.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(max(1, fps)))
        config.queueDepth = 5
        config.pixelFormat = kCVPixelFormatType_32BGRA
        config.showsCursor = captureCursor
        config.scalesToFit = false
        if #available(macOS 14.0, *) {
            config.preservesAspectRatio = true
        }
        config.capturesAudio = false
        config.colorSpaceName = CGColorSpace.sRGB

        let filter = SCContentFilter(display: display, excludingWindows: [])
        let queue = DispatchQueue(label: "macss.capture", qos: .userInitiated)
        let engine = CaptureEngine(info: info, queue: queue)
        let stream = SCStream(filter: filter, configuration: config, delegate: engine)
        engine.stream = stream
        try stream.addStreamOutput(engine, type: .screen, sampleHandlerQueue: queue)
        return engine
    }

    func start() async throws {
        try await stream.startCapture()
    }

    func stop() async {
        try? await stream.stopCapture()
    }

    /// Ask for a fresh keyframe. ScreenCaptureKit only emits IDRs on demand when
    /// the stream is reconfigured, so we nudge the frame interval briefly.
    func requestKeyframe() {
        // The encoder handles keyframe forcing; this is a hook for future use.
    }

    // MARK: SCStreamOutput

    func stream(_ stream: SCStream,
                didOutputSampleBuffer sampleBuffer: CMSampleBuffer,
                of type: SCStreamOutputType) {
        guard type == .screen, sampleBuffer.isValid else { return }
        guard let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }

        // Drop non-complete frames (idle/blank) reported by ScreenCaptureKit.
        if let status = frameStatus(from: sampleBuffer), status != .complete {
            return
        }

        let pts = CMSampleBufferGetPresentationTimeStamp(sampleBuffer)
        delegate?.captureEngine(self, didOutput: pixelBuffer, pts: pts)
    }

    private func frameStatus(from sampleBuffer: CMSampleBuffer) -> SCFrameStatus? {
        guard let attachments = CMSampleBufferGetSampleAttachmentsArray(
            sampleBuffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
            let first = attachments.first,
            let raw = first[frameStatusKey] as? Int
        else { return nil }
        return SCFrameStatus(rawValue: raw)
    }

    // MARK: SCStreamDelegate

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        delegate?.captureEngine(self, didStopWith: error)
    }
}

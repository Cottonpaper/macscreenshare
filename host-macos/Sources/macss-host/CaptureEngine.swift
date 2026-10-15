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
    /// Minimum seconds between forwarded idle frames, or `nil` to disable the
    /// idle refresh entirely.
    private let idleInterval: CFTimeInterval?
    /// Retained so an idle frame can be captured on demand. ScreenCaptureKit's
    /// stream goes silent when nothing changes, so we pull a fresh frame (which
    /// includes the settled cursor) ourselves.
    private let filter: SCContentFilter
    private let configuration: SCStreamConfiguration
    private var idleTimer: DispatchSourceTimer?
    private var lastForwarded: CFTimeInterval = 0

    private init(
        info: DisplayInfo,
        filter: SCContentFilter,
        configuration: SCStreamConfiguration,
        queue: DispatchQueue,
        idleInterval: CFTimeInterval?
    ) {
        self.info = info
        self.filter = filter
        self.configuration = configuration
        self.sampleQueue = queue
        self.idleInterval = idleInterval
    }

    /// Build a capture engine for the main display (falling back to any display).
    ///
    /// `idleRefreshFPS` controls how often ScreenCaptureKit's "idle" frames
    /// (nothing has changed) are still forwarded, so a static screen keeps the
    /// client in sync instead of freezing on the last frame. `0` drops them.
    static func mainDisplay(
        fps: Int, captureCursor: Bool = true, idleRefreshFPS: Int = 10
    ) async throws -> CaptureEngine {
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
        let idleInterval: CFTimeInterval? =
            idleRefreshFPS > 0 ? 1.0 / Double(idleRefreshFPS) : nil
        let engine = CaptureEngine(
            info: info, filter: filter, configuration: config,
            queue: queue, idleInterval: idleInterval)
        Log.info("capture: \(display.width)x\(display.height), idle refresh \(idleRefreshFPS) fps")
        let stream = SCStream(filter: filter, configuration: config, delegate: engine)
        engine.stream = stream
        try stream.addStreamOutput(engine, type: .screen, sampleHandlerQueue: queue)
        return engine
    }

    func start() async throws {
        try await stream.startCapture()
        startIdleTimer()
    }

    func stop() async {
        idleTimer?.cancel()
        idleTimer = nil
        try? await stream.stopCapture()
    }

    /// ScreenCaptureKit stops delivering frames while the screen is static, so
    /// the client freezes on a stale frame (whose cursor lags behind the real
    /// one). When the stream has been quiet for `idleInterval`, pull a fresh
    /// frame ourselves — which includes the settled cursor — and forward it.
    private func startIdleTimer() {
        guard let idleInterval, idleInterval > 0 else { return }
        let timer = DispatchSource.makeTimerSource(queue: sampleQueue)
        timer.schedule(deadline: .now() + idleInterval, repeating: idleInterval)
        timer.setEventHandler { [weak self] in
            guard let self, let interval = self.idleInterval else { return }
            guard CFAbsoluteTimeGetCurrent() - self.lastForwarded >= interval else { return }
            self.captureIdleFrame()
        }
        timer.resume()
        idleTimer = timer
    }

    private func captureIdleFrame() {
        guard #available(macOS 14.0, *) else { return }
        // Claim the slot up front so the timer doesn't stack captures while one
        // is still in flight.
        lastForwarded = CFAbsoluteTimeGetCurrent()
        SCScreenshotManager.captureSampleBuffer(
            contentFilter: filter, configuration: configuration
        ) { [weak self] sampleBuffer, error in
            guard let self else { return }
            self.sampleQueue.async {
                if let error {
                    Log.debug("idle frame capture failed: \(error)")
                    return
                }
                guard let sampleBuffer,
                      let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
                let pts = CMSampleBufferGetPresentationTimeStamp(sampleBuffer)
                self.delegate?.captureEngine(self, didOutput: pixelBuffer, pts: pts)
            }
        }
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

        // ScreenCaptureKit marks frames as `idle` when nothing changed. Dropping
        // them all makes the client freeze on the last frame, so forward idle
        // frames at a low rate to keep the picture converged (cursor, caret,
        // unrecognised updates). `blank`/`suspended` are still dropped.
        if let status = frameStatus(from: sampleBuffer) {
            let now = CFAbsoluteTimeGetCurrent()
            switch status {
            case .complete:
                lastForwarded = now
            case .idle:
                guard let idleInterval else { return }
                if now - lastForwarded < idleInterval { return }
                lastForwarded = now
            default:
                return
            }
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

import AppKit
import CoreGraphics
import CoreMedia
import CoreVideo
import Foundation
import Network

struct Settings {
    var token: String?
    var controlPort: UInt16 = DefaultPort.control
    var fps: Int = 60
    var bitrateKbps: Int = 20_000
    var captureCursor: Bool = true
    /// Forward ScreenCaptureKit "idle" frames at up to this rate (0 = drop them).
    var idleRefreshFPS: Int = 10
    var hostName: String = Host.current().localizedName ?? "Mac"
}

/// Accepts one client and drives capture -> encode -> UDP.
final class HostServer {
    private let settings: Settings
    private let queue = DispatchQueue(label: "macss.server")
    private var listener: NWListener?
    private var session: Session?

    init(settings: Settings) {
        self.settings = settings
    }

    func start() throws {
        let parameters = NWParameters.tcp
        parameters.allowLocalEndpointReuse = true
        guard let port = NWEndpoint.Port(rawValue: settings.controlPort) else {
            throw HostError.invalidArgument("bad control port \(settings.controlPort)")
        }
        let listener = try NWListener(using: parameters, on: port)

        listener.newConnectionHandler = { [weak self] connection in
            guard let self else { return }
            // Printed the instant a TCP connection reaches the process. If a
            // client connects but this never appears, the packets are being
            // dropped before they get here (firewall, network filter, or the
            // Local Network permission).
            Log.info("inbound connection accepted from \(connection.endpoint)")
            self.queue.async { self.accept(connection) }
        }

        listener.stateUpdateHandler = { [weak self] state in
            let configured = self?.settings.controlPort ?? 0
            switch state {
            case .setup:
                Log.info("control listener: setup")
            case .waiting(let error):
                // The system is holding the listener back. The port can still
                // show up in `lsof` while inbound SYNs are silently dropped.
                Log.warn("control listener: WAITING on tcp/\(configured) — \(error)")
                Log.warn("  inbound connections will not be accepted in this state")
            case .ready:
                let bound = listener.port.map { "\($0)" } ?? "?"
                Log.info("control listener: ready on tcp/\(configured) (bound \(bound))")
                let addresses = HostServer.localIPv4Addresses()
                Log.info("  local IPv4: \(addresses.isEmpty ? "none" : addresses.joined(separator: ", "))")
            case .failed(let error):
                Log.warn("control listener: failed — \(error)")
            case .cancelled:
                Log.info("control listener: cancelled")
            @unknown default:
                Log.info("control listener: unknown state")
            }
        }
        listener.start(queue: queue)
        self.listener = listener
    }

    /// Every non-loopback IPv4 address, for logging and diagnostics.
    static func localIPv4Addresses() -> [String] {
        var results: [String] = []
        var ifaddr: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&ifaddr) == 0, let first = ifaddr else { return results }
        defer { freeifaddrs(ifaddr) }

        var pointer = first
        while true {
            let interface = pointer.pointee
            if let addr = interface.ifa_addr, addr.pointee.sa_family == UInt8(AF_INET) {
                var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
                if getnameinfo(addr, socklen_t(addr.pointee.sa_len),
                               &host, socklen_t(host.count),
                               nil, 0, NI_NUMERICHOST) == 0 {
                    let name = String(cString: interface.ifa_name)
                    let address = String(cString: host)
                    if !address.hasPrefix("127.") {
                        results.append("\(address) (\(name))")
                    }
                }
            }
            guard let next = interface.ifa_next else { break }
            pointer = next
        }
        return results
    }

    func stop() {
        queue.async {
            self.session?.close()
            self.session = nil
            self.listener?.cancel()
            self.listener = nil
        }
    }

    private func accept(_ connection: NWConnection) {
        if let existing = session, !existing.isClosed {
            Log.warn("rejecting extra connection: a session is already active")
            if let framed = try? ControlFraming.frame(
                HostMessage.error(code: ErrorCode.busy, message: "host is already streaming").encoded()) {
                connection.start(queue: queue)
                connection.send(content: framed, completion: .contentProcessed { _ in
                    connection.cancel()
                })
            } else {
                connection.cancel()
            }
            return
        }

        Log.info("client connected from \(connection.endpoint)")
        let session = Session(connection: connection, settings: settings, queue: queue)
        self.session = session
        session.onClosed = { [weak self] in
            self?.queue.async {
                self?.session = nil
                Log.info("session ended")
            }
        }
        session.start()
    }
}

// MARK: - Session

final class Session: NSObject, CaptureEngineDelegate, VideoEncoderDelegate {
    let connection: NWConnection
    let settings: Settings
    let queue: DispatchQueue
    private(set) var isClosed = false
    var onClosed: (() -> Void)?

    private let peerHost: String
    private var frameDecoder = ControlFrameDecoder()
    private var lastActivity = Date()
    private var nonce: UInt64 = 0

    private let lock = NSLock()
    private var videoSender: UDPSender?
    private var encoder: VideoEncoder?
    private var capture: CaptureEngine?
    private var injector: InputInjector?
    private var clipboard: ClipboardBridge?
    /// Held while streaming so the Mac does not idle-sleep mid-session.
    private var activityToken: NSObjectProtocol?

    private var codec: Codec = .h264
    private var fps = 60
    private var bitrateKbps = 20_000

    private var heartbeat: DispatchSourceTimer?
    private var statsTimer: DispatchSourceTimer?
    private let clipboardQueue = DispatchQueue(label: "macss.clipboard")

    private var frameID: UInt32 = 0
    private var statsFrames = 0
    private var statsBytes = 0
    private var statsEncodeMs: Float = 0
    private var statsWindowStart = Date()

    init(connection: NWConnection, settings: Settings, queue: DispatchQueue) {
        self.connection = connection
        self.settings = settings
        self.queue = queue
        self.peerHost = Session.peerHost(from: connection) ?? "127.0.0.1"
        super.init()
    }

    func start() {
        connection.stateUpdateHandler = { [weak self] state in
            switch state {
            case .failed(let error):
                Log.warn("connection failed: \(error)")
                self?.close()
            case .cancelled:
                self?.close()
            default:
                break
            }
        }
        connection.start(queue: queue)
        receiveLoop()
        startHeartbeat()
    }

    func close() {
        guard !isClosed else { return }
        isClosed = true

        heartbeat?.cancel(); heartbeat = nil
        statsTimer?.cancel(); statsTimer = nil
        clipboard?.stop(); clipboard = nil
        endActivity()

        let engine = capture
        engine?.delegate = nil
        capture = nil
        if let engine { Task { await engine.stop() } }

        lock.lock()
        encoder?.invalidate()
        encoder = nil
        videoSender = nil
        lock.unlock()

        injector = nil
        connection.cancel()
        onClosed?()
    }

    // MARK: Receive

    private func receiveLoop() {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 1 << 16) {
            [weak self] data, _, isComplete, error in
            guard let self else { return }
            if let data, !data.isEmpty {
                self.lastActivity = Date()
                do {
                    for frame in try self.frameDecoder.append(data) {
                        try self.handle(frame)
                    }
                } catch {
                    Log.warn("control error: \(error)")
                    self.sendError(code: ErrorCode.badRequest, message: "\(error)")
                    self.close()
                    return
                }
            }
            if error != nil || isComplete {
                self.close()
                return
            }
            self.receiveLoop()
        }
    }

    private func handle(_ frame: Data) throws {
        let message = try ClientMessage.decode(frame)
        switch message {
        case .hello(let hello):
            handleHello(hello)
        case .ping(let value):
            send(.pong(nonce: value))
        case .pong:
            break
        case .startStream(let request):
            startStream(request)
        case .stopStream:
            stopStream()
        case .input(let event):
            injector?.handle(event)
        case .clipboard(let text):
            clipboard?.applyRemote(text)
        case .requestKeyframe:
            lock.lock(); let enc = encoder; lock.unlock()
            enc?.requestKeyframe()
        case .setBitrate(let request):
            setBitrate(request)
        case .bye:
            send(.bye)
            close()
        }
    }

    private func handleHello(_ hello: Hello) {
        guard hello.protocolVersion == ProtocolVersion.current else {
            sendError(code: ErrorCode.protocolMismatch,
                      message: "host speaks protocol \(ProtocolVersion.current)")
            close()
            return
        }
        if let required = settings.token, hello.token != required {
            sendError(code: ErrorCode.unauthorized, message: "invalid or missing token")
            close()
            return
        }

        let displayID = CGMainDisplayID()
        let width = CGDisplayPixelsWide(displayID)
        let height = CGDisplayPixelsHigh(displayID)
        let scale = Float(NSScreen.main?.backingScaleFactor ?? 1)

        Log.info("hello from \(hello.clientName) (\(hello.protocolVersion))")
        send(.helloAck(HelloAck(
            protocolVersion: ProtocolVersion.current,
            sessionId: UUID().uuidString,
            hostName: settings.hostName,
            displayWidth: UInt32(max(0, width)),
            displayHeight: UInt32(max(0, height)),
            scale: scale,
            videoPort: DefaultPort.video,
            codecs: [.h264, .hevc],
            capabilities: ["adaptive_bitrate"]
        )))
    }

    // MARK: Streaming

    private func startStream(_ request: StartStreamRequest) {
        guard videoSenderIsNil() else {
            Log.debug("start_stream ignored: already streaming")
            return
        }

        codec = request.codec
        fps = min(max(Int(request.fps), 1), 120)
        bitrateKbps = min(max(Int(request.maxBitrateKbps), 500), 200_000)
        let udpPort = request.udpPort

        guard Permissions.hasScreenRecording else {
            let error = HostError.missingPermission("Screen Recording")
            Log.warn("\(error); grant it from the menu bar, then relaunch the host")
            sendError(code: ErrorCode.badRequest, message: "\(error) on host")
            close()
            return
        }
        if !Permissions.hasAccessibility {
            Log.warn("\(HostError.missingPermission("Accessibility")); remote mouse/keyboard input will be ignored")
        }

        Task { [weak self] in
            guard let self else { return }
            do {
                let engine = try await CaptureEngine.mainDisplay(
                    fps: self.fps,
                    captureCursor: self.settings.captureCursor,
                    idleRefreshFPS: self.settings.idleRefreshFPS)
                self.queue.async { self.activate(engine: engine, udpPort: udpPort) }
            } catch {
                self.queue.async {
                    self.sendError(code: ErrorCode.badRequest,
                                   message: "capture setup failed: \(error)")
                    self.close()
                }
            }
        }
    }

    private func activate(engine: CaptureEngine, udpPort: UInt16) {
        guard videoSenderIsNil() else {
            Task { await engine.stop() }
            return
        }
        do {
            let sender = try UDPSender(host: peerHost, port: udpPort)
            let encoder = try VideoEncoder(
                width: engine.info.widthPixels,
                height: engine.info.heightPixels,
                fps: fps,
                bitrateKbps: bitrateKbps,
                codec: codec)
            encoder.delegate = self
            engine.delegate = self

            lock.lock()
            self.videoSender = sender
            self.encoder = encoder
            lock.unlock()

            self.capture = engine
            self.injector = InputInjector(displayID: engine.info.displayID)

            let clipboard = ClipboardBridge()
            clipboard.onText = { [weak self] text in
                self?.send(.clipboard(text: text))
            }
            clipboard.start(queue: clipboardQueue)
            self.clipboard = clipboard

            lock.lock()
            statsWindowStart = Date()
            lock.unlock()

            beginActivity()

            Log.info("streaming to \(peerHost):\(udpPort) — \(codec.rawValue) \(engine.info.widthPixels)x\(engine.info.heightPixels) @ \(fps) fps")
            send(.streamStarted(StreamStarted(
                codec: codec,
                width: UInt32(engine.info.widthPixels),
                height: UInt32(engine.info.heightPixels),
                fps: UInt32(fps))))

            Task { try? await engine.start() }
        } catch {
            Log.warn("could not start streaming: \(error)")
            sendError(code: ErrorCode.badRequest, message: "\(error)")
            close()
        }
    }

    private func stopStream() {
        let engine = capture
        engine?.delegate = nil
        capture = nil
        if let engine { Task { await engine.stop() } }

        lock.lock()
        encoder?.invalidate()
        encoder = nil
        videoSender = nil
        lock.unlock()

        clipboard?.stop()
        clipboard = nil
        injector = nil
        endActivity()
        Log.info("stream stopped")
    }

    private func videoSenderIsNil() -> Bool {
        lock.lock(); defer { lock.unlock() }
        return videoSender == nil
    }

    /// Apply a client-requested bitrate change (adaptive bitrate).
    private func setBitrate(_ request: SetBitrateRequest) {
        let kbps = min(max(Int(request.maxBitrateKbps), 500), 200_000)
        lock.lock(); let enc = encoder; lock.unlock()
        guard let enc else {
            Log.debug("set_bitrate ignored: not streaming")
            return
        }
        enc.setBitrate(kbps: kbps)
        Log.info("client changed bitrate to \(kbps) kbit/s")
    }

    // MARK: Sleep prevention

    /// Keep the Mac (and its display) awake for as long as a stream is active.
    /// Without this, an idle Mac can sleep mid-session and drop the client.
    private func beginActivity() {
        guard activityToken == nil else { return }
        activityToken = ProcessInfo.processInfo.beginActivity(
            options: [.userInitiated, .idleDisplaySleepDisabled, .idleSystemSleepDisabled],
            reason: "Streaming the screen to a connected client")
        Log.debug("holding sleep-prevention assertion while streaming")
    }

    private func endActivity() {
        guard let token = activityToken else { return }
        ProcessInfo.processInfo.endActivity(token)
        activityToken = nil
        Log.debug("released sleep-prevention assertion")
    }

    // MARK: Timers

    private func startHeartbeat() {
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now() + 2, repeating: 2)
        timer.setEventHandler { [weak self] in
            guard let self else { return }
            if Date().timeIntervalSince(self.lastActivity) > 10 {
                Log.info("control heartbeat timed out")
                self.close()
                return
            }
            self.nonce &+= 1
            self.send(.ping(nonce: self.nonce))
        }
        timer.resume()
        heartbeat = timer

        let stats = DispatchSource.makeTimerSource(queue: queue)
        stats.schedule(deadline: .now() + 1, repeating: 1)
        stats.setEventHandler { [weak self] in self?.emitStats() }
        stats.resume()
        statsTimer = stats
    }

    private func emitStats() {
        lock.lock()
        let frames = statsFrames
        let bytes = statsBytes
        let encodeSum = statsEncodeMs
        let elapsed = max(Date().timeIntervalSince(statsWindowStart), 0.001)
        statsFrames = 0
        statsBytes = 0
        statsEncodeMs = 0
        statsWindowStart = Date()
        let streaming = videoSender != nil
        lock.unlock()

        guard streaming else { return }
        let measuredFPS = Float(Double(frames) / elapsed)
        let kbps = UInt32((Double(bytes) * 8.0 / elapsed / 1000.0).rounded())
        let avgEncode = frames > 0 ? encodeSum / Float(frames) : 0
        send(.stats(StatsMessage(fps: measuredFPS, bitrateKbps: kbps,
                                 encodeMs: avgEncode, dropped: 0)))
    }

    // MARK: CaptureEngineDelegate

    func captureEngine(_ engine: CaptureEngine, didOutput pixelBuffer: CVPixelBuffer, pts: CMTime) {
        lock.lock(); let enc = encoder; lock.unlock()
        enc?.encode(pixelBuffer: pixelBuffer, pts: pts)
    }

    func captureEngine(_ engine: CaptureEngine, didStopWith error: Error?) {
        if let error { Log.warn("capture stopped: \(error)") }
        queue.async { self.close() }
    }

    // MARK: VideoEncoderDelegate

    func encoder(_ encoder: VideoEncoder, didEncodeFrame data: Data,
                 keyframe: Bool, ptsMs: UInt32, encodeMs: Float) {
        lock.lock()
        guard let sender = videoSender else { lock.unlock(); return }
        let id = frameID
        frameID &+= 1
        statsFrames += 1
        statsBytes += data.count
        statsEncodeMs += encodeMs
        lock.unlock()

        let datagrams = VideoFraming.packetize(
            frameID: id, codec: codec, keyframe: keyframe,
            timestampMs: ptsMs, payload: data)
        for datagram in datagrams {
            sender.send(datagram)
        }
    }

    func encoder(_ encoder: VideoEncoder, didFail error: Error?) {
        Log.warn("encoder failed: \(error.map(String.init(describing:)) ?? "unknown")")
    }

    // MARK: Send

    private func send(_ message: HostMessage) {
        guard !isClosed else { return }
        do {
            let framed = try ControlFraming.frame(message.encoded())
            connection.send(content: framed, completion: .contentProcessed { error in
                if let error { Log.debug("control send failed: \(error)") }
            })
        } catch {
            Log.warn("could not encode control message: \(error)")
        }
    }

    private func sendError(code: String, message: String) {
        send(.error(code: code, message: message))
    }

    private static func peerHost(from connection: NWConnection) -> String? {
        if case let .hostPort(host, _) = connection.endpoint {
            return "\(host)"
        }
        if let remote = connection.currentPath?.remoteEndpoint,
           case let .hostPort(host, _) = remote {
            return "\(host)"
        }
        return nil
    }
}

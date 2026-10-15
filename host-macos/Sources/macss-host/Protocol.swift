import Foundation

/// Wire protocol types. Mirrors `docs/protocol.md` and the Rust
/// `macss-protocol` crate. Keep field names in sync via the CodingKeys.

enum ProtocolVersion {
    static let current: UInt16 = 1
}

enum DefaultPort {
    static let control: UInt16 = 47810
    static let video: UInt16 = 47811
}

enum Codec: String, Codable, CaseIterable {
    case h264
    case hevc

    /// Wire id used in the video header.
    var wireId: UInt8 {
        switch self {
        case .h264: return 0
        case .hevc: return 1
        }
    }
}

enum MouseButton: String, Codable {
    case left, right, middle, back, forward
}

enum ModifierBits {
    static let shift: UInt8 = 1 << 0
    static let ctrl: UInt8 = 1 << 1
    static let alt: UInt8 = 1 << 2
    static let meta: UInt8 = 1 << 3
}

enum ProtocolError: Error, CustomStringConvertible {
    case unknownMessageType(String)
    case unknownKind(String)
    case malformed(String)
    case notEncodable
    case frameTooLarge(Int)

    var description: String {
        switch self {
        case .unknownMessageType(let t): return "unknown message type \(t)"
        case .unknownKind(let k): return "unknown input kind \(k)"
        case .malformed(let m): return "malformed message: \(m)"
        case .notEncodable: return "message is not encodable on this side"
        case .frameTooLarge(let n): return "control frame too large: \(n) bytes"
        }
    }
}

// MARK: - Inbound (client -> host)

/// A single input event. Only decoded on the host.
enum InputEvent {
    case mouseMove(x: Float, y: Float)
    case mouseButton(button: MouseButton, down: Bool, x: Float, y: Float)
    case mouseScroll(dx: Float, dy: Float)
    case key(code: UInt16, down: Bool, modifiers: UInt8)
}

extension InputEvent: Decodable {
    private enum CodingKeys: String, CodingKey {
        case kind, x, y, button, down, dx, dy, code, modifiers
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let kind = try c.decode(String.self, forKey: .kind)
        switch kind {
        case "mouse_move":
            self = .mouseMove(x: try c.decode(Float.self, forKey: .x),
                              y: try c.decode(Float.self, forKey: .y))
        case "mouse_button":
            self = .mouseButton(button: try c.decode(MouseButton.self, forKey: .button),
                                down: try c.decode(Bool.self, forKey: .down),
                                x: try c.decode(Float.self, forKey: .x),
                                y: try c.decode(Float.self, forKey: .y))
        case "mouse_scroll":
            self = .mouseScroll(dx: try c.decode(Float.self, forKey: .dx),
                                dy: try c.decode(Float.self, forKey: .dy))
        case "key":
            self = .key(code: try c.decode(UInt16.self, forKey: .code),
                        down: try c.decode(Bool.self, forKey: .down),
                        modifiers: try c.decode(UInt8.self, forKey: .modifiers))
        default:
            throw ProtocolError.unknownKind(kind)
        }
    }
}

struct Hello: Decodable {
    var type: String
    var protocolVersion: UInt16
    var clientName: String
    var token: String?

    enum CodingKeys: String, CodingKey {
        case type
        case protocolVersion = "protocol"
        case clientName = "client_name"
        case token
    }
}

struct StartStreamRequest: Decodable {
    var type: String
    var codec: Codec
    var fps: UInt32
    var maxBitrateKbps: UInt32
    var udpPort: UInt16

    enum CodingKeys: String, CodingKey {
        case type, codec, fps
        case maxBitrateKbps = "max_bitrate_kbps"
        case udpPort = "udp_port"
    }
}

struct Nonce: Decodable {
    var nonce: UInt64
}

struct ClipboardIn: Decodable {
    var text: String
}

/// `set_bitrate`: change the target bitrate mid-stream.
struct SetBitrateRequest: Decodable {
    var type: String
    var maxBitrateKbps: UInt32

    enum CodingKeys: String, CodingKey {
        case type
        case maxBitrateKbps = "max_bitrate_kbps"
    }
}

/// A message received from the client.
enum ClientMessage {
    case hello(Hello)
    case ping(nonce: UInt64)
    case pong(nonce: UInt64)
    case startStream(StartStreamRequest)
    case stopStream
    case input(InputEvent)
    case clipboard(text: String)
    case requestKeyframe
    case setBitrate(SetBitrateRequest)
    case bye
}

extension ClientMessage {
    private struct Envelope: Decodable { let type: String }

    static func decode(_ data: Data) throws -> ClientMessage {
        let decoder = JSONDecoder()
        let env = try decoder.decode(Envelope.self, from: data)
        switch env.type {
        case "hello":
            return .hello(try decoder.decode(Hello.self, from: data))
        case "ping":
            return .ping(nonce: try decoder.decode(Nonce.self, from: data).nonce)
        case "pong":
            return .pong(nonce: try decoder.decode(Nonce.self, from: data).nonce)
        case "start_stream":
            return .startStream(try decoder.decode(StartStreamRequest.self, from: data))
        case "stop_stream":
            return .stopStream
        case "input":
            let input = try decoder.decode(InputWrapper.self, from: data)
            return .input(input.event)
        case "clipboard":
            return .clipboard(text: try decoder.decode(ClipboardIn.self, from: data).text)
        case "request_keyframe":
            return .requestKeyframe
        case "set_bitrate":
            return .setBitrate(try decoder.decode(SetBitrateRequest.self, from: data))
        case "bye":
            return .bye
        default:
            throw ProtocolError.unknownMessageType(env.type)
        }
    }
}

private struct InputWrapper: Decodable { let event: InputEvent }

// MARK: - Outbound (host -> client)

struct HelloAck: Encodable {
    var type = "hello_ack"
    var protocolVersion: UInt16
    var sessionId: String
    var hostName: String
    var displayWidth: UInt32
    var displayHeight: UInt32
    var scale: Float
    var videoPort: UInt16
    var codecs: [Codec]
    /// Additive capability list; clients ignore unknown entries.
    var capabilities: [String]

    enum CodingKeys: String, CodingKey {
        case type
        case protocolVersion = "protocol"
        case sessionId = "session_id"
        case hostName = "host_name"
        case displayWidth = "display_width"
        case displayHeight = "display_height"
        case scale
        case videoPort = "video_port"
        case codecs
        case capabilities
    }
}

struct StreamStarted: Encodable {
    var type = "stream_started"
    var codec: Codec
    var width: UInt32
    var height: UInt32
    var fps: UInt32
}

struct StatsMessage: Encodable {
    var type = "stats"
    var fps: Float
    var bitrateKbps: UInt32
    var encodeMs: Float
    var dropped: UInt32

    enum CodingKeys: String, CodingKey {
        case type, fps
        case bitrateKbps = "bitrate_kbps"
        case encodeMs = "encode_ms"
        case dropped
    }
}

struct ErrorMessage: Encodable {
    var type = "error"
    var code: String
    var message: String
}

struct NonceMessage: Encodable {
    var type: String
    var nonce: UInt64
}

struct ClipboardOut: Encodable {
    var type = "clipboard"
    var text: String
}

struct EmptyMessage: Encodable {
    var type: String
}

/// A message sent from the host to the client.
enum HostMessage {
    case helloAck(HelloAck)
    case error(code: String, message: String)
    case ping(nonce: UInt64)
    case pong(nonce: UInt64)
    case streamStarted(StreamStarted)
    case clipboard(text: String)
    case stats(StatsMessage)
    case bye
}

extension HostMessage {
    func encoded() throws -> Data {
        let encoder = JSONEncoder()
        switch self {
        case .helloAck(let m): return try encoder.encode(m)
        case .error(let code, let message):
            return try encoder.encode(ErrorMessage(code: code, message: message))
        case .ping(let nonce):
            return try encoder.encode(NonceMessage(type: "ping", nonce: nonce))
        case .pong(let nonce):
            return try encoder.encode(NonceMessage(type: "pong", nonce: nonce))
        case .streamStarted(let m): return try encoder.encode(m)
        case .clipboard(let text): return try encoder.encode(ClipboardOut(text: text))
        case .stats(let m): return try encoder.encode(m)
        case .bye: return try encoder.encode(EmptyMessage(type: "bye"))
        }
    }
}

enum ErrorCode {
    static let protocolMismatch = "protocol_mismatch"
    static let unauthorized = "unauthorized"
    static let busy = "busy"
    static let badRequest = "bad_request"
}

// MARK: - Control framing

enum ControlFraming {
    static let maxFrameLength = 8 * 1024 * 1024

    /// Prepend the big-endian length prefix to an encoded message.
    static func frame(_ payload: Data) throws -> Data {
        guard payload.count <= maxFrameLength else {
            throw ProtocolError.frameTooLarge(payload.count)
        }
        var out = Data(capacity: 4 + payload.count)
        var len = UInt32(payload.count).bigEndian
        withUnsafeBytes(of: &len) { out.append(contentsOf: $0) }
        out.append(payload)
        return out
    }
}

/// Incremental length-prefixed frame decoder for an async byte stream.
struct ControlFrameDecoder {
    private var buffer = Data()

    enum DecodeError: Error {
        case frameTooLarge(Int)
    }

    /// Append bytes and yield any complete payloads.
    mutating func append(_ data: Data) throws -> [Data] {
        buffer.append(data)
        var frames: [Data] = []
        while buffer.count >= 4 {
            let len = buffer.withUnsafeBytes { raw -> UInt32 in
                let b = raw.bindMemory(to: UInt8.self)
                return (UInt32(b[0]) << 24) | (UInt32(b[1]) << 16)
                    | (UInt32(b[2]) << 8) | UInt32(b[3])
            }
            guard len <= UInt32(ControlFraming.maxFrameLength) else {
                throw DecodeError.frameTooLarge(Int(len))
            }
            let total = 4 + Int(len)
            if buffer.count < total { break }
            let start = buffer.startIndex + 4
            let end = buffer.startIndex + total
            frames.append(Data(buffer[start..<end]))
            buffer.removeFirst(total)
        }
        return frames
    }
}

// MARK: - Video framing

enum VideoFraming {
    static let headerLength = 16
    static let chunkLength = 1384
    static let maxDatagram = headerLength + chunkLength

    enum Flags {
        static let keyframe: UInt8 = 1 << 0
        static let lastChunk: UInt8 = 1 << 1
    }

    /// Split an encoded Annex-B frame into datagrams with the 16-byte header.
    static func packetize(
        frameID: UInt32,
        codec: Codec,
        keyframe: Bool,
        timestampMs: UInt32,
        payload: Data
    ) -> [Data] {
        let chunks: [Data]
        if payload.isEmpty {
            chunks = [Data()]
        } else {
            chunks = stride(from: 0, to: payload.count, by: chunkLength).map {
                payload.subdata(in: $0..<min($0 + chunkLength, payload.count))
            }
        }
        let count = UInt16(clamping: chunks.count)

        return chunks.enumerated().map { index, chunk in
            var flags: UInt8 = 0
            if keyframe && index == 0 { flags |= Flags.keyframe }
            if index == chunks.count - 1 { flags |= Flags.lastChunk }

            var header = Data(capacity: headerLength)
            header.append(contentsOf: withUnsafeBytes(of: frameID.bigEndian) { Array($0) })
            header.append(contentsOf: withUnsafeBytes(of: UInt16(index).bigEndian) { Array($0) })
            header.append(contentsOf: withUnsafeBytes(of: count.bigEndian) { Array($0) })
            header.append(flags)
            header.append(codec.wireId)
            header.append(contentsOf: [UInt8(0), UInt8(0)]) // reserved
            header.append(contentsOf: withUnsafeBytes(of: timestampMs.bigEndian) { Array($0) })
            header.append(chunk)
            return header
        }
    }
}

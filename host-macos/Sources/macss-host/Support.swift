import Foundation

enum Log {
    static var verbose = false

    static func info(_ message: String) { emit("INFO", message) }
    static func warn(_ message: String) { emit("WARN", message) }

    static func debug(_ message: String) {
        if verbose { emit("DEBUG", message) }
    }

    private static func emit(_ level: String, _ message: String) {
        let now = Date().timeIntervalSince1970
        let line = String(format: "[%10.3f] %@ %@\n", now, level, message)
        FileHandle.standardError.write(Data(line.utf8))
    }
}

enum HostError: Error, CustomStringConvertible {
    case noDisplay
    case encoderCreate(OSStatus)
    case missingPermission(String)
    case network(String)
    case invalidArgument(String)

    var description: String {
        switch self {
        case .noDisplay: return "no capturable display found"
        case .encoderCreate(let s): return "could not create the video encoder (status \(s))"
        case .missingPermission(let p): return "missing \(p) permission"
        case .network(let m): return "network error: \(m)"
        case .invalidArgument(let m): return "invalid argument: \(m)"
        }
    }
}

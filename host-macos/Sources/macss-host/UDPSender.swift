import Darwin
import Foundation

/// Minimal UDP sender over BSD sockets, used for video datagrams.
final class UDPSender {
    private let fd: Int32
    private var address: sockaddr_in

    init(host: String, port: UInt16) throws {
        let fd = socket(AF_INET, SOCK_DGRAM, 0)
        guard fd >= 0 else {
            throw HostError.network("socket() failed: \(errno)")
        }
        self.fd = fd

        var address = sockaddr_in()
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_family = sa_family_t(AF_INET)
        address.sin_port = port.bigEndian
        guard inet_pton(AF_INET, host, &address.sin_addr) == 1 else {
            close(fd)
            throw HostError.network("invalid IPv4 address \(host)")
        }
        self.address = address

        // A large send buffer reduces drops when a keyframe bursts.
        var sendBuffer: Int32 = 1 << 20
        setsockopt(fd, SOL_SOCKET, SO_SNDBUF, &sendBuffer,
                   socklen_t(MemoryLayout<Int32>.size))
    }

    func send(_ data: Data) {
        data.withUnsafeBytes { raw in
            guard let base = raw.baseAddress else { return }
            _ = withUnsafePointer(to: &address) { pointer in
                pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { sockaddrPointer in
                    sendto(fd,
                           base,
                           raw.count,
                           0,
                           sockaddrPointer,
                           socklen_t(MemoryLayout<sockaddr_in>.size))
                }
            }
        }
    }

    deinit {
        close(fd)
    }
}

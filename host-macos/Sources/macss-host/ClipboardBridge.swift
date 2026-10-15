import AppKit
import Foundation

/// Watches the local pasteboard and applies text received from the client.
///
/// v1 supports plain text only. `changeCount` is tracked so an applied remote
/// value is not echoed straight back to the sender.
final class ClipboardBridge {
    private let pasteboard = NSPasteboard.general
    private var lastChangeCount: Int
    private var timer: DispatchSourceTimer?

    /// Called on the clipboard queue when the local pasteboard changes.
    var onText: ((String) -> Void)?

    init() {
        lastChangeCount = NSPasteboard.general.changeCount
    }

    func start(queue: DispatchQueue, interval: TimeInterval = 0.5) {
        guard timer == nil else { return }
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now() + interval, repeating: interval)
        timer.setEventHandler { [weak self] in self?.poll() }
        timer.resume()
        self.timer = timer
    }

    func stop() {
        timer?.cancel()
        timer = nil
    }

    /// Apply text that arrived from the client without echoing it back.
    func applyRemote(_ text: String) {
        if pasteboard.string(forType: .string) == text { return }
        pasteboard.clearContents()
        pasteboard.setString(text, forType: .string)
        lastChangeCount = pasteboard.changeCount
    }

    private func poll() {
        let count = pasteboard.changeCount
        guard count != lastChangeCount else { return }
        lastChangeCount = count
        guard let text = pasteboard.string(forType: .string), !text.isEmpty else { return }
        onText?(text)
    }
}

import AppKit
import ApplicationServices
import CoreGraphics
import Foundation

/// macOS TCC permission helpers.
enum Permissions {
    static var hasScreenRecording: Bool {
        CGPreflightScreenCaptureAccess()
    }

    static var hasAccessibility: Bool {
        AXIsProcessTrusted()
    }

    /// Ask for Screen Recording (shows the system prompt once).
    @discardableResult
    static func requestScreenRecording() -> Bool {
        CGRequestScreenCaptureAccess()
    }

    /// Ask for Accessibility (shows the system prompt once).
    static func requestAccessibility() {
        let options = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true]
        _ = AXIsProcessTrustedWithOptions(options as CFDictionary)
    }

    static func openSettings(_ pane: Pane) {
        let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?\(pane.anchor)")!
        NSWorkspace.shared.open(url)
    }

    enum Pane {
        case screenRecording
        case accessibility

        var anchor: String {
            switch self {
            case .screenRecording: return "Privacy_ScreenCapture"
            case .accessibility: return "Privacy_Accessibility"
            }
        }
    }
}

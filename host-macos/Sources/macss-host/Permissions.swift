import AppKit
import ApplicationServices
import CoreGraphics
import Foundation

/// macOS TCC permission helpers.
enum Permissions {
    /// Note: the Screen Recording result is fixed for the process lifetime;
    /// a newly granted permission only takes effect after a relaunch.
    static var hasScreenRecording: Bool {
        CGPreflightScreenCaptureAccess()
    }

    static var hasAccessibility: Bool {
        AXIsProcessTrusted()
    }

    /// True when running from inside a `.app` bundle. When run as a bare
    /// binary (e.g. `swift run`), TCC attributes grants to the parent shell.
    static var isBundled: Bool {
        Bundle.main.bundleIdentifier != nil && Bundle.main.bundlePath.hasSuffix(".app")
    }

    private static let screenPromptedKey = "macss.prompted.screenRecording"
    private static let accessibilityPromptedKey = "macss.prompted.accessibility"

    /// Ask for Screen Recording. The system prompt only appears once per app
    /// identity, so after the first attempt open System Settings instead.
    static func requestScreenRecording() {
        let defaults = UserDefaults.standard
        if !defaults.bool(forKey: screenPromptedKey) {
            defaults.set(true, forKey: screenPromptedKey)
            // Don't also open Settings here; it would stack on top of the prompt.
            _ = CGRequestScreenCaptureAccess()
            return
        }
        openSettings(.screenRecording)
    }

    /// Ask for Accessibility, falling back to System Settings after the first prompt.
    static func requestAccessibility() {
        let defaults = UserDefaults.standard
        if !defaults.bool(forKey: accessibilityPromptedKey) {
            defaults.set(true, forKey: accessibilityPromptedKey)
            // Use the literal key rather than the SDK constant: its Swift type
            // varies (CFString vs Unmanaged<CFString>) across toolchains.
            let options = ["AXTrustedCheckOptionPrompt": true] as CFDictionary
            _ = AXIsProcessTrustedWithOptions(options)
            return
        }
        openSettings(.accessibility)
    }

    static func openSettings(_ pane: Pane) {
        let candidates = [
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?\(pane.anchor)",
            "x-apple.systempreferences:com.apple.preference.security?\(pane.anchor)",
        ]
        for string in candidates {
            if let url = URL(string: string), NSWorkspace.shared.open(url) { return }
        }
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

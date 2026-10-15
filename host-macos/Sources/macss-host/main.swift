import AppKit
import Darwin
import Foundation

// MARK: - Arguments

struct Arguments {
    var settings = Settings()
    var verbose = false
    var showHelp = false

    static let usage = """
    macss-host — share this Mac's screen with a macscreenshare client

    USAGE:
        macss-host [OPTIONS]

    OPTIONS:
        --token <T>      Require clients to present this shared secret
        --port <P>       TCP control port                 [default: 47810]
        --fps <N>        Default frame rate               [default: 60]
        --bitrate <K>    Default max bitrate in kbit/s    [default: 20000]
        --no-cursor      Do not capture the mouse cursor
        --verbose        Verbose logging
        -h, --help       Show this help
    """

    static func parse(_ argv: [String]) -> Arguments {
        var result = Arguments()
        let args = Array(argv)
        var i = 0
        while i < args.count {
            switch args[i] {
            case "--token":
                if i + 1 < args.count { result.settings.token = args[i + 1]; i += 1 }
            case "--port":
                if i + 1 < args.count, let value = UInt16(args[i + 1]) {
                    result.settings.controlPort = value
                }
                i += 1
            case "--fps":
                if i + 1 < args.count, let value = Int(args[i + 1]) {
                    result.settings.fps = min(max(value, 1), 120)
                }
                i += 1
            case "--bitrate":
                if i + 1 < args.count, let value = Int(args[i + 1]) {
                    result.settings.bitrateKbps = max(value, 500)
                }
                i += 1
            case "--no-cursor":
                result.settings.captureCursor = false
            case "--verbose":
                result.verbose = true
            case "-h", "--help":
                result.showHelp = true
            default:
                Log.warn("ignoring unknown argument \(args[i])")
            }
            i += 1
        }
        return result
    }
}

// MARK: - App delegate

final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuDelegate {
    private var statusItem: NSStatusItem?
    private var server: HostServer?
    private var settings = Settings()

    func applicationDidFinishLaunching(_ notification: Notification) {
        let arguments = Arguments.parse(Array(CommandLine.arguments.dropFirst()))
        if arguments.showHelp {
            print(Arguments.usage)
            NSApp.terminate(nil)
            return
        }

        settings = arguments.settings
        Log.verbose = arguments.verbose

        configureStatusItem()
        checkPermissions()
        startServer()
    }

    // MARK: Menu bar

    private func configureStatusItem() {
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        item.button?.title = "🖥️"
        item.button?.toolTip = "MacScreenShare"
        statusItem = item
        rebuildMenu()
    }

    private func rebuildMenu() {
        let menu = statusItem?.menu ?? NSMenu()
        populate(menu)
        menu.delegate = self
        statusItem?.menu = menu
    }

    private func populate(_ menu: NSMenu) {
        menu.removeAllItems()
        let address = localIPv4Address() ?? "your-mac.local"
        let state = server != nil ? "listening on \(address):\(settings.controlPort)" : "stopped"
        _ = menu.addItem(withTitle: state, action: nil, keyEquivalent: "")
        menu.addItem(.separator())

        if server == nil {
            _ = menu.addItem(withTitle: "Start server", action: #selector(startServerAction), keyEquivalent: "")
        } else {
            _ = menu.addItem(withTitle: "Stop server", action: #selector(stopServerAction), keyEquivalent: "")
        }

        menu.addItem(.separator())
        let screenOK = Permissions.hasScreenRecording
        let axOK = Permissions.hasAccessibility
        _ = menu.addItem(withTitle: screenOK ? "✅ Screen Recording granted" : "⚠️ Grant Screen Recording…",
                         action: screenOK ? nil : #selector(openScreenRecording), keyEquivalent: "")
        _ = menu.addItem(withTitle: axOK ? "✅ Accessibility granted" : "⚠️ Grant Accessibility…",
                         action: axOK ? nil : #selector(openAccessibility), keyEquivalent: "")
        if !screenOK {
            // Screen Recording grants only apply to a fresh process.
            _ = menu.addItem(withTitle: "Relaunch to apply permissions",
                             action: #selector(relaunch), keyEquivalent: "")
        }
        if !Permissions.isBundled {
            _ = menu.addItem(withTitle: "⚠️ Not running as .app — grants go to the shell",
                             action: nil, keyEquivalent: "")
        }
        _ = menu.addItem(withTitle: "Copy host address",
                         action: #selector(copyAddress), keyEquivalent: "")
        menu.addItem(.separator())
        _ = menu.addItem(withTitle: "Quit", action: #selector(quit), keyEquivalent: "q")

        for item in menu.items where item.action != nil {
            item.target = self
        }
    }

    func menuWillOpen(_ menu: NSMenu) {
        // Re-check permission state each time the menu is shown.
        populate(menu)
    }

    @objc private func relaunch() {
        stopServer()
        let path = Bundle.main.bundlePath
        if Permissions.isBundled {
            let task = Process()
            task.executableURL = URL(fileURLWithPath: "/bin/sh")
            task.arguments = ["-c", "sleep 1; /usr/bin/open -n \"$0\"", path]
            try? task.run()
        }
        NSApp.terminate(nil)
    }

    @objc private func startServerAction() { startServer() }
    @objc private func stopServerAction() { stopServer(); rebuildMenu() }

    @objc private func openScreenRecording() {
        Permissions.requestScreenRecording()
    }

    @objc private func openAccessibility() {
        Permissions.requestAccessibility()
    }

    @objc private func copyAddress() {
        let address = "\(localIPv4Address() ?? "127.0.0.1"):\(settings.controlPort)"
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(address, forType: .string)
    }

    @objc private func quit() {
        stopServer()
        NSApp.terminate(nil)
    }

    // MARK: Server lifecycle

    private func startServer() {
        guard server == nil else { return }
        let server = HostServer(settings: settings)
        do {
            try server.start()
            self.server = server
            let address = localIPv4Address() ?? "127.0.0.1"
            Log.info("ready — point the client at \(address):\(settings.controlPort)")
            if settings.token != nil {
                Log.info("a shared token is required")
            } else {
                Log.warn("no --token set; any LAN client may connect")
            }
        } catch {
            Log.warn("could not start: \(error)")
        }
        rebuildMenu()
    }

    private func stopServer() {
        server?.stop()
        server = nil
        rebuildMenu()
    }

    private func checkPermissions() {
        // Check only — do not pop a system prompt at launch. Requesting here
        // fires before the app has any reason to capture or inject, and when
        // the host is launched from a shell (rather than via
        // `open dist/MacScreenShare.app`) macOS attributes that prompt to the
        // shell, e.g. "Terminal". Grant from the menu bar, and let
        // ScreenCaptureKit prompt on the first capture.
        if !Permissions.isBundled {
            Log.warn("not running from a .app bundle; macOS will attribute permissions to the launching shell. Use scripts/build-host-app.sh and `open dist/MacScreenShare.app`")
        }
        if !Permissions.hasScreenRecording {
            Log.warn("Screen Recording permission is missing; grant it from the menu bar")
        }
        if !Permissions.hasAccessibility {
            Log.warn("Accessibility permission is missing; grant it from the menu bar to enable keyboard/mouse control")
        }
    }

    // MARK: Helpers

    private func localIPv4Address() -> String? {
        var result: String?
        var ifaddr: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&ifaddr) == 0, let first = ifaddr else { return nil }
        defer { freeifaddrs(ifaddr) }

        var pointer = first
        while true {
            let interface = pointer.pointee
            if let addr = interface.ifa_addr, addr.pointee.sa_family == UInt8(AF_INET) {
                let name = String(cString: interface.ifa_name)
                if name == "en0" || name == "en1" {
                    var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
                    if getnameinfo(addr, socklen_t(addr.pointee.sa_len),
                                   &host, socklen_t(host.count),
                                   nil, 0, NI_NUMERICHOST) == 0 {
                        result = String(cString: host)
                        break
                    }
                }
            }
            guard let next = interface.ifa_next else { break }
            pointer = next
        }
        return result
    }
}

// MARK: - Entry point

let application = NSApplication.shared
let delegate = AppDelegate()
application.delegate = delegate
_ = application.setActivationPolicy(.accessory)
application.run()

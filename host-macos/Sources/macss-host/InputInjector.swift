import CoreGraphics
import Foundation

/// Injects synthetic mouse and keyboard events into the local session.
///
/// Requires the Accessibility permission (System Settings -> Privacy &
/// Security -> Accessibility). Some secure-input contexts (password prompts,
/// some games) intentionally ignore synthetic events.
final class InputInjector {
    private let displayID: CGDirectDisplayID
    private let bounds: CGRect

    init(displayID: CGDirectDisplayID) {
        self.displayID = displayID
        self.bounds = CGDisplayBounds(displayID)
    }

    func handle(_ event: InputEvent) {
        switch event {
        case .mouseMove(let x, let y):
            move(x: x, y: y, button: .left, type: .mouseMoved)

        case .mouseButton(let button, let down, let x, let y):
            let cgButton = Self.cgButton(button)
            let type: CGEventType
            switch (button, down) {
            case (.left, true): type = .leftMouseDown
            case (.left, false): type = .leftMouseUp
            case (.right, true): type = .rightMouseDown
            case (.right, false): type = .rightMouseUp
            default: type = down ? .otherMouseDown : .otherMouseUp
            }
            move(x: x, y: y, button: cgButton, type: type)

        case .mouseScroll(let dx, let dy):
            guard let event = CGEvent(
                scrollWheelEvent2Source: nil,
                units: .line,
                wheelCount: 2,
                wheel1: Int32(-dy.rounded()),
                wheel2: Int32(dx.rounded()),
                wheel3: 0
            ) else { return }
            event.post(tap: .cghidEventTap)

        case .key(let code, let down, let modifiers):
            guard let keyCode = KeyMapping.keyCode(forHID: code) else {
                Log.debug("input: unmapped HID key 0x\(String(code, radix: 16))")
                return
            }
            guard let event = CGEvent(
                keyboardEventSource: nil,
                virtualKey: keyCode,
                keyDown: down
            ) else { return }
            event.flags = Self.flags(from: modifiers)
            event.post(tap: .cghidEventTap)
        }
    }

    private func move(x: Float, y: Float, button: CGMouseButton, type: CGEventType) {
        let point = CGPoint(
            x: bounds.origin.x + CGFloat(clamp01(x)) * bounds.width,
            y: bounds.origin.y + CGFloat(clamp01(y)) * bounds.height
        )
        guard let event = CGEvent(
            mouseEventSource: nil,
            mouseType: type,
            mouseCursorPosition: point,
            mouseButton: button
        ) else { return }
        event.post(tap: .cghidEventTap)
    }

    private func clamp01(_ v: Float) -> Float {
        min(max(v, 0), 1)
    }

    private static func cgButton(_ button: MouseButton) -> CGMouseButton {
        switch button {
        case .left: return .left
        case .right: return .right
        case .middle: return .center
        case .back: return CGMouseButton(rawValue: 3) ?? .center
        case .forward: return CGMouseButton(rawValue: 4) ?? .center
        }
    }

    private static func flags(from modifiers: UInt8) -> CGEventFlags {
        var flags: CGEventFlags = []
        if modifiers & ModifierBits.shift != 0 { flags.insert(.maskShift) }
        if modifiers & ModifierBits.ctrl != 0 { flags.insert(.maskControl) }
        if modifiers & ModifierBits.alt != 0 { flags.insert(.maskAlternate) }
        if modifiers & ModifierBits.meta != 0 { flags.insert(.maskCommand) }
        return flags
    }
}

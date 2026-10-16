import AppKit
import SwiftUI
import UserNotifications

@main
struct PullDayApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var state = AppState.shared
    @StateObject private var prefs = Prefs.shared

    var body: some Scene {
        MenuBarExtra {
            MenuContentView()
                .environmentObject(state)
                .environmentObject(prefs)
        } label: {
            MenuBarLabel(count: state.updatesAvailable.count)
        }
        .menuBarExtraStyle(.window)
    }
}

struct MenuBarLabel: View {
    let count: Int

    var body: some View {
        Image(nsImage: MenuBarLabel.glyph)
        if count > 0 { Text("\(count)") }
    }

    /// The app icon's barbell and down arrow, drawn on a 24-point grid (Resources/AppIcon.svg is the full icon).
    /// A template image, so macOS tints it for light and dark menu bars.
    static let glyph: NSImage = {
        let image = NSImage(size: NSSize(width: 18, height: 18), flipped: true) { _ in
            let scale = NSAffineTransform()
            scale.scale(by: 18.0 / 24.0)
            scale.concat()
            NSColor.black.set()
            for plate in [NSRect(x: 3.2, y: 4.6, width: 1.8, height: 4.8), NSRect(x: 5.6, y: 3.4, width: 2.2, height: 7.2),
                          NSRect(x: 16.2, y: 3.4, width: 2.2, height: 7.2), NSRect(x: 19, y: 4.6, width: 1.8, height: 4.8)] {
                NSBezierPath(roundedRect: plate, xRadius: 0.5, yRadius: 0.5).fill()
            }
            let strokes: [([NSPoint], CGFloat)] = [
                ([NSPoint(x: 5, y: 7), NSPoint(x: 19, y: 7)], 1.6),                              // bar
                ([NSPoint(x: 12, y: 9.6), NSPoint(x: 12, y: 19)], 2.2),                          // arrow shaft
                ([NSPoint(x: 8.4, y: 15.6), NSPoint(x: 12, y: 19.2), NSPoint(x: 15.6, y: 15.6)], 2.2), // arrow head
            ]
            for (points, width) in strokes {
                let path = NSBezierPath()
                path.lineWidth = width
                path.lineCapStyle = .round
                path.lineJoinStyle = .round
                path.move(to: points[0])
                points.dropFirst().forEach { path.line(to: $0) }
                path.stroke()
            }
            return true
        }
        image.isTemplate = true
        return image
    }()
}

final class AppDelegate: NSObject, NSApplicationDelegate, UNUserNotificationCenterDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        UNUserNotificationCenter.current().delegate = self
        Notifier.requestAuthorization()
        Task { @MainActor in AppState.shared.start() }
    }

    // Show banners even though PullDay is technically "active".
    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter,
                                            willPresent notification: UNNotification,
                                            withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
        completionHandler([.banner, .sound])
    }
}

enum Notifier {
    static func requestAuthorization() {
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound]) { _, _ in }
    }

    static func send(title: String, body: String) {
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        let request = UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request)
    }
}

/// Plain AppKit windows for Settings and the log. Scene-based windows behave
/// unpredictably in a menu-bar-only app, so these are managed by hand.
@MainActor
final class WindowManager {
    static let shared = WindowManager()
    private var windows: [String: NSWindow] = [:]

    func showSettings(tab: SettingsTab? = nil) {
        if let tab { AppState.shared.settingsTab = tab }
        show(id: "settings", title: "PullDay Settings", size: NSSize(width: 580, height: 480), resizable: false) {
            SettingsView()
        }
    }

    func showLog() {
        show(id: "log", title: "PullDay Log", size: NSSize(width: 640, height: 460), resizable: true) {
            LogView()
        }
    }

    private func show<Content: View>(id: String, title: String, size: NSSize, resizable: Bool,
                                     @ViewBuilder content: () -> Content) {
        if let window = windows[id] {
            window.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }
        let root = content()
            .environmentObject(AppState.shared)
            .environmentObject(Prefs.shared)
        let window = NSWindow(contentViewController: NSHostingController(rootView: root))
        window.title = title
        window.styleMask = resizable ? [.titled, .closable, .miniaturizable, .resizable] : [.titled, .closable]
        window.setContentSize(size)
        window.isReleasedWhenClosed = false
        window.center()
        windows[id] = window
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }
}

// SwiftUI also declares a `State` macro whose plugin ships only with Xcode, not the Command Line Tools; the alias selects the property wrapper.
typealias StateWrapper<Value> = SwiftUI.State<Value>

import LocalAuthentication
import Observation
import SwiftUI
import UIKit

/// Face ID / Touch ID / passcode lock. Asks when the app opens and after five
/// minutes in the background (on by default, only while a server is saved),
/// and hides the app behind a cover in the app switcher.
@MainActor
@Observable
final class AppLock {
    /// How long the app may stay in the background without locking.
    static let timeout: TimeInterval = 5 * 60
    private static let enabledKey = "appLockEnabled"

    private(set) var enabled: Bool
    private(set) var locked: Bool
    /// Hides the app while it's in the background.
    private(set) var covered = false
    private(set) var authenticating = false

    @ObservationIgnored private var hasServers: Bool
    @ObservationIgnored private var backgroundedAt: Date?
    /// Prompt once when the app (re)opens locked; not again after the person
    /// cancels Face ID (which also makes the scene active again).
    @ObservationIgnored private var promptWhenActive: Bool

    init(hasServers: Bool) {
        let enabled = UserDefaults.standard.object(forKey: Self.enabledKey) as? Bool ?? true
        self.enabled = enabled
        self.hasServers = hasServers
        locked = enabled && hasServers
        promptWhenActive = enabled && hasServers
    }

    /// Whether the lock screen (or the background cover) should be shown.
    var isShielded: Bool { locked || covered }

    /// "Face ID", "Touch ID", "Optic ID" or "Passcode".
    var methodName: String {
        let context = LAContext()
        _ = context.canEvaluatePolicy(.deviceOwnerAuthenticationWithBiometrics, error: nil)
        switch context.biometryType {
        case .faceID: return "Face ID"
        case .touchID: return "Touch ID"
        case .opticID: return "Optic ID"
        default: return "Passcode"
        }
    }

    var systemImage: String {
        switch methodName {
        case "Face ID": "faceid"
        case "Touch ID": "touchid"
        case "Optic ID": "opticid"
        default: "lock.fill"
        }
    }

    func serversChanged(hasServers: Bool) {
        self.hasServers = hasServers
        if !hasServers {
            locked = false
            covered = false
        }
    }

    // MARK: Scene phases

    func didEnterBackground() {
        guard enabled, hasServers else { return }
        backgroundedAt = Date()
        covered = true
    }

    func didBecomeActive() {
        if let since = backgroundedAt, enabled, hasServers,
            Date().timeIntervalSince(since) >= Self.timeout
        {
            locked = true
            promptWhenActive = true
        }
        backgroundedAt = nil
        covered = false
        if locked, promptWhenActive {
            promptWhenActive = false
            Task { await unlock() }
        }
    }

    // MARK: Authentication

    /// Asks for Face ID or the passcode; unlocks on success.
    func unlock() async {
        guard locked, !authenticating else { return }
        if await authenticate(reason: "Unlock difu to see and control your agents.") {
            locked = false
        }
    }

    /// Turning the lock off needs the owner too.
    func setEnabled(_ on: Bool) async {
        guard on != enabled else { return }
        if !on, !(await authenticate(reason: "Turn off the difu app lock.")) { return }
        enabled = on
        UserDefaults.standard.set(on, forKey: Self.enabledKey)
    }

    private func authenticate(reason: String) async -> Bool {
        let context = LAContext()
        var error: NSError?
        guard context.canEvaluatePolicy(.deviceOwnerAuthentication, error: &error) else {
            // No passcode is set, so there is nothing to check against.
            return true
        }
        authenticating = true
        defer { authenticating = false }
        do {
            return try await context.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: reason)
        } catch {
            return false
        }
    }
}

/// Shows the lock screen in its own window above everything else, including
/// sheets, alerts and the keyboard's host window.
@MainActor
enum LockShield {
    private static var window: UIWindow?

    static func update(visible: Bool, lock: AppLock) {
        if visible {
            show(lock)
        } else {
            hide()
        }
    }

    /// Hides the shield and hands key status back to the app's window, so
    /// text fields in it and its sheets get the keyboard again.
    private static func hide() {
        guard let shield = window else { return }
        shield.isHidden = true
        window = nil
        shield.windowScene?.windows
            .first { $0 !== shield && !$0.isHidden && $0.windowLevel == .normal }?
            .makeKey()
    }

    private static func show(_ lock: AppLock) {
        guard window == nil,
            let scene = UIApplication.shared.connectedScenes
                .compactMap({ $0 as? UIWindowScene })
                .first(where: { $0.activationState != .unattached })
        else { return }
        UIApplication.shared.sendAction(
            #selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
        let shield = UIWindow(windowScene: scene)
        shield.windowLevel = .alert + 1
        let host = UIHostingController(rootView: LockScreen().environment(lock).themed())
        host.view.backgroundColor = .systemBackground
        shield.rootViewController = host
        shield.isHidden = false
        window = shield
    }
}

/// The lock screen: app mark plus an Unlock button (also the background cover).
struct LockScreen: View {
    @Environment(AppLock.self) private var lock

    var body: some View {
        VStack(spacing: 20) {
            Spacer()
            Image(systemName: "chevron.right.square.fill")
                .font(.system(size: 64))
                .foregroundStyle(Color.accent)
                .symbolRenderingMode(.hierarchical)
            Text("difu")
                .font(.system(size: 34, weight: .bold, design: .monospaced))
            Spacer()
            if lock.locked {
                Button {
                    Task { await lock.unlock() }
                } label: {
                    Label("Unlock with \(lock.methodName)", systemImage: lock.systemImage)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 6)
                }
                .buttonStyle(.borderedProminent)
                .foregroundStyle(Color.onAccent)
                .controlSize(.large)
                .disabled(lock.authenticating)
                .padding(.horizontal, 32)
            }
        }
        .padding(.bottom, 48)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.screenBackground)
    }
}

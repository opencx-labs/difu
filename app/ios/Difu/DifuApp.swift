import DifuKit
import SwiftUI

@main
struct DifuApp: App {
    @State private var app = AppModel()
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(app)
                .environment(app.lock)
                .themed()
                .onOpenURL { app.open($0) }
                .onChange(of: scenePhase, initial: true) { _, phase in
                    switch phase {
                    case .active:
                        app.activate()
                        app.lock.didBecomeActive()
                    case .background:
                        app.lock.didEnterBackground()
                        app.deactivate()
                    default: break
                    }
                }
                .onChange(of: app.lock.isShielded, initial: true) { _, shielded in
                    LockShield.update(visible: shielded, lock: app.lock)
                }
        }
    }
}

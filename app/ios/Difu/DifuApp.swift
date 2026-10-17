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
                .onOpenURL { app.open($0) }
                .onChange(of: scenePhase, initial: true) { _, phase in
                    switch phase {
                    case .active: app.activate()
                    case .background: app.deactivate()
                    default: break
                    }
                }
        }
    }
}

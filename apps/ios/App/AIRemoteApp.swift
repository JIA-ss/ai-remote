import SwiftUI
import AIRemotePhone

@MainActor @main struct AIRemoteApp: App {
    private let store = PhoneStore()
    @Environment(\.scenePhase) private var phase
    var body: some Scene {
        WindowGroup {
            PhoneRootView(store: store)
                .task { await store.resume() }
                .onChange(of: phase) { _, value in
                    Task { if value == .active { await store.resume() } else if value == .background { await store.suspend() } }
                }
        }
    }
}

import SwiftUI
import AIRemotePhone
import UIKit

@MainActor @main struct AIRemoteApp: App {
    private let store = PhoneStore()
    @Environment(\.scenePhase) private var phase
    var body: some Scene {
        WindowGroup {
            PhoneRootView(store: store)
                .task { await store.resume() }
                .onChange(of: phase) { _, value in
                    if value == .active { Task { await store.resume() } }
                    else if value == .background { finishBackgroundSession() }
                }
        }
    }
    private func finishBackgroundSession() {
        let cleanup = BackgroundCleanup()
        cleanup.ticket = UIApplication.shared.beginBackgroundTask(withName: "Finish remote session") { Task { @MainActor in cleanup.end() } }
        Task { await store.suspend(); cleanup.end() }
    }
}

@MainActor private final class BackgroundCleanup {
    var ticket = UIBackgroundTaskIdentifier.invalid
    func end() {
        guard ticket != .invalid else { return }
        UIApplication.shared.endBackgroundTask(ticket)
        ticket = .invalid
    }
}

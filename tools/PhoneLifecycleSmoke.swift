import Foundation
import AIRemotePhone

@main struct PhoneLifecycleSmoke {
    @MainActor static func main() async throws {
        let directory = CommandLine.arguments[1]
        let client = PhoneClient(directory: directory + "/client")
        try await client.start()
        let identity = try await client.request(["op": .string("status")])["device"]
        for _ in 0..<3 {
            async let stopped: Void = client.close()
            try await Task.sleep(for: .milliseconds(20))
            try await client.start()
            await stopped
            let reopened = try await client.request(["op": .string("status")])["device"]
            guard reopened == identity else { throw NSError(domain: "lifecycle", code: 1) }
        }
        await client.close()
        let store = PhoneStore(directory: directory + "/store")
        await store.resume()
        let device = store.device
        for _ in 0..<3 {
            async let backgrounded: Void = store.suspend()
            try await Task.sleep(for: .milliseconds(20))
            await store.resume()
            await backgrounded
            try await store.refresh()
            guard store.error == nil, store.device == device, !device.isEmpty else { throw NSError(domain: "lifecycle", code: 2) }
        }
        await store.suspend()
        print("{\"result\":\"passed\",\"client_close_start_races\":3,\"store_background_resume_races\":3,\"scope\":\"native Mac Swift/Rust core, no phone install\"}")
    }
}

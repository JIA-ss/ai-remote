import XCTest
@testable import AIRemotePhone

/// The route a host reports while connected, and the note shown when P2P failed.
final class PhoneRouteTests: XCTestCase {
    private func host(_ fields: [String: JSONValue]) -> PhoneHost {
        PhoneHost(.object(["name": .string("office-pc")].merging(fields) { $1 }))
    }

    func testDirectRouteHasNoRelayNote() {
        let h = host(["path": .string("direct")])
        XCTAssertEqual(h.routeLabel, "直连")
        XCTAssertNil(h.relayNote)
    }

    func testN0RelaySaysP2PFailedAndNamesTheRelay() throws {
        let h = host(["path": .string("relay"), "relay": .string("https://aps1-1.relay.n0.iroh.link."), "relay_n0": .bool(true)])
        XCTAssertEqual(h.relay, "aps1-1.relay.n0.iroh.link")
        XCTAssertEqual(h.routeLabel, "n0 公共中继")
        let note = try XCTUnwrap(h.relayNote)
        XCTAssertTrue(note.note.contains("P2P 直连失败"))
        XCTAssertTrue(note.detail.contains("aps1-1.relay.n0.iroh.link"))
    }

    func testSelfHostedRelayIsNotCalledN0() throws {
        let h = host(["path": .string("relay"), "relay": .string("https://relay.example.org:8443"), "relay_n0": .bool(false)])
        XCTAssertEqual(h.relay, "relay.example.org")
        XCTAssertEqual(h.routeLabel, "中继")
        XCTAssertFalse(try XCTUnwrap(h.relayNote).note.contains("n0"))
    }

    func testUnknownRouteShowsNothing() {
        let h = host([:])
        XCTAssertNil(h.routeLabel)
        XCTAssertNil(h.relayNote)
    }
}

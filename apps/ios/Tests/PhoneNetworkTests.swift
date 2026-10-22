import XCTest
@testable import AIRemotePhone

/// The Network sheet's reading of the core's settings, and the address rules it shares with the
/// desktop app (`tools/network-ui.test.mjs` has the same cases).
final class PhoneNetworkTests: XCTestCase {
    func testANewPhoneIsQuietAndAutomatic() {
        let net = PhoneNetwork(.object(["mode": .string("auto"), "lan_discovery": .bool(false), "port_mapping": .bool(false),
                                        "locked": .bool(false), "pair_code": .bool(true), "lan_port": .number(47800)]))
        XCTAssertFalse(net.lan)
        XCTAssertFalse(net.lanDiscovery)
        XCTAssertFalse(net.portMapping)
        XCTAssertTrue(net.pairCode)
        XCTAssertEqual(net.lanPort, 47800)
    }

    func testLocalNetworkOnlyWithoutDiscoveryHasNoPairCode() {
        let net = PhoneNetwork(.object(["mode": .string("lan"), "pair_code": .bool(false), "locked": .bool(true)]))
        XCTAssertTrue(net.lan)
        XCTAssertFalse(net.pairCode)
        XCTAssertTrue(net.locked)
    }

    func testTheCoreMessagesAboutTheNetworkAreExplained() {
        let refused = "no pair code on the local network only with local discovery off: pair with the ticket, or turn local discovery on"
        XCTAssertTrue(PhoneNetwork.explain(refused).contains("完整票据"))
        XCTAssertTrue(PhoneNetwork.explain("host 'office-pc' did not answer", lan: true).contains("核对它的地址"))
        XCTAssertEqual(PhoneNetwork.explain("host 'office-pc' did not answer"), "host 'office-pc' did not answer")
    }

    func testHostsCarryTheirAddresses() {
        let host = PhoneHost(.object(["name": .string("office-pc"), "addresses": .array([.string("192.168.1.20:47800")])]))
        XCTAssertEqual(host.addresses, ["192.168.1.20:47800"])
        XCTAssertEqual(PhoneHost(.object(["name": .string("old")])).addresses, [])
    }

    func testShowsIPv4FirstAndHidesAddressesThatOnlyWorkOnOneDevice() {
        let got = PhoneAddress.shareable(["[fe80::1c2:3:4:5]:47800", "127.0.0.1:47800", "[2001:db8::7]:47800",
                                          "192.168.1.20:47800", "169.254.10.2:47800", "192.168.1.20:47800"])
        XCTAssertEqual(got, ["192.168.1.20:47800", "[2001:db8::7]:47800"])
    }

    func testTakesIPAddressesWithOrWithoutAPortSeveralAtOnce() {
        for ok in ["192.168.1.20", "192.168.1.20:47800", "10.0.0.5, 10.0.0.6:50000", "[fe80::1]:9", "::1"] {
            XCTAssertTrue(PhoneAddress.validInput(ok), ok)
        }
    }

    func testRefusesNamesEmptyInputAndImpossibleNumbers() {
        for bad in ["", "  , ", "office-pc", "192.168.1", "300.1.1.1", "10.0.0.5:0", "10.0.0.5:70000", "http://10.0.0.5",
                    "10.0.0.5:", "[fe80::1", "１９２.168.1.20"] {
            XCTAssertFalse(PhoneAddress.validInput(bad), bad)
        }
    }
}

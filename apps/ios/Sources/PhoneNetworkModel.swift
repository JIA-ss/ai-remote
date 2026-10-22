import Foundation

/// This phone's network settings, as the core reports them (`network_settings`): how it reaches
/// computers, and the two extras that put traffic on the network beyond the connections.
public struct PhoneNetwork: Sendable, Equatable {
    /// Direct on the local network only: no relay, no address lookup; computers are dialled at their
    /// stored addresses.
    public let lan: Bool
    /// Finds computers on the local network (mDNS queries; the phone never announces itself).
    public let lanDiscovery: Bool
    /// Asks the router to open a port (UPnP, NAT-PMP, PCP).
    public let portMapping: Bool
    /// `quiet = true` keeps both extras off.
    public let locked: Bool
    /// Whether a pair code can be found: on the local network only, only local discovery finds one.
    public let pairCode: Bool
    /// The port a computer listens on on the local network only, when an address has none.
    public let lanPort: Int
    public init(_ v: JSONValue) {
        lan = v["mode"] == .string("lan")
        lanDiscovery = v["lan_discovery"] == .bool(true)
        portMapping = v["port_mapping"] == .bool(true)
        locked = v["locked"] == .bool(true)
        pairCode = v["pair_code"] != .bool(false)
        lanPort = Int(v["lan_port"].number ?? 47800)
    }

    /// The core's message, in the app's words where it is about the network settings.
    public static func explain(_ message: String, lan: Bool = false) -> String {
        if message.contains("no pair code on the local network only") {
            return "仅限局域网且没打开“在局域网里自动发现电脑”时，配对码找不到电脑。请改用完整票据，或到 网络 里打开自动发现。"
        }
        if lan, ["did not answer", "timed out", "no address", "unreachable"].contains(where: message.contains) {
            return "\(message)\n\n仅限局域网：确认电脑和这台 iPhone 在同一网络，电脑上也选了“仅限局域网”，并在 网络 里核对它的地址。"
        }
        return message
    }
}

/// Addresses as the Network sheet shows and accepts them, the same rules as the desktop app.
public enum PhoneAddress {
    /// `ip`, `ip:port` or `[ipv6]:port` split into its IP (brackets removed) and port; `nil` when the
    /// brackets or the port are malformed.
    static func split(_ address: String) -> (host: String, port: String?)? {
        if address.hasPrefix("[") {
            guard let close = address.firstIndex(of: "]") else { return nil }
            let host = String(address[address.index(after: address.startIndex)..<close])
            let rest = address[address.index(after: close)...]
            if rest.isEmpty { return (host, nil) }
            return rest.hasPrefix(":") ? (host, String(rest.dropFirst())) : nil
        }
        // One colon is IPv4 with a port; more is a bare IPv6 address.
        guard address.filter({ $0 == ":" }).count == 1, let colon = address.lastIndex(of: ":") else { return (address, nil) }
        return (String(address[..<colon]), String(address[address.index(after: colon)...]))
    }
    static func isIPv4(_ host: String) -> Bool {
        let parts = host.split(separator: ".", omittingEmptySubsequences: false)
        return parts.count == 4 && parts.allSatisfy { (1...3).contains($0.count) && isDigits($0) && Int($0)! <= 255 }
    }
    static func isIPv6(_ host: String) -> Bool {
        host.contains(":") && host.allSatisfy { $0.isASCII && ($0.isHexDigit || $0 == ":" || $0 == ".") }
    }
    private static func isDigits(_ text: Substring) -> Bool { !text.isEmpty && text.allSatisfy { $0.isASCII && $0.isNumber } }
    /// The addresses worth showing: no loopback or link-local ones, IPv4 first, duplicates removed.
    public static func shareable(_ addresses: [String]) -> [String] {
        let keep = addresses.filter { a in
            let h = (split(a)?.host ?? a).lowercased()
            return !(h.hasPrefix("127.") || h == "::1" || h.hasPrefix("fe80:") || h.hasPrefix("169.254."))
        }
        let v4 = keep.filter { isIPv4(split($0)?.host ?? $0) }
        var seen = Set<String>()
        return (v4 + keep.filter { !v4.contains($0) }).filter { seen.insert($0).inserted }
    }
    /// Whether `text` is one or more IP addresses (`ip`, `ip:port`, `[ipv6]:port`, separated by
    /// commas or spaces), the form the core takes. Names are not looked up, so they are refused.
    public static func validInput(_ text: String) -> Bool {
        let parts = text.split(whereSeparator: { $0 == "," || $0.isWhitespace })
        return !parts.isEmpty && parts.allSatisfy { part in
            guard let (host, port) = split(String(part)) else { return false }
            if let port, !(isDigits(Substring(port)) && (1...65535).contains(Int(port) ?? 0)) { return false }
            return isIPv4(host) || isIPv6(host)
        }
    }
}

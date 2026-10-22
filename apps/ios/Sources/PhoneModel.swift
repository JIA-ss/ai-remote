import Foundation
import CoreGraphics

public enum JSONValue: Codable, Sendable, Equatable {
    case object([String: JSONValue]), array([JSONValue]), string(String), number(Double), bool(Bool), null
    public init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let x = try? c.decode(Bool.self) { self = .bool(x) }
        else if let x = try? c.decode(Double.self) { self = .number(x) }
        else if let x = try? c.decode(String.self) { self = .string(x) }
        else if let x = try? c.decode([JSONValue].self) { self = .array(x) }
        else { self = .object(try c.decode([String: JSONValue].self)) }
    }
    public func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .object(let x): try c.encode(x)
        case .array(let x): try c.encode(x)
        case .string(let x): try c.encode(x)
        case .number(let x): try c.encode(x)
        case .bool(let x): try c.encode(x)
        case .null: try c.encodeNil()
        }
    }
    public subscript(_ key: String) -> JSONValue { if case .object(let v) = self { return v[key] ?? .null }; return .null }
    public var string: String? { if case .string(let v) = self { return v }; return nil }
    public var number: Double? { if case .number(let v) = self { return v }; return nil }
    public var array: [JSONValue] { if case .array(let v) = self { return v }; return [] }
    public var text: String { String(data: (try? JSONEncoder().encode(self)) ?? Data(), encoding: .utf8) ?? "null" }
    public static func parse(_ text: String) throws -> JSONValue { try JSONDecoder().decode(Self.self, from: Data(text.utf8)) }
}

public struct PhoneHost: Identifiable, Sendable, Equatable {
    public var id: String { name }
    public let name: String
    public let level: String?
    /// "direct", "relay" or "unknown"; known only while a connection is open.
    public let path: String?
    /// The relay host carrying the traffic, when relayed and known.
    public let relay: String?
    /// The relay is one of n0's public relays (rate-limited, meant by n0 for development).
    public let relayN0: Bool
    /// While relayed, what likely kept the connection from going direct.
    public let p2pObstacle: String?
    /// Where this phone dials the computer first (`ip:port`): stored at pairing, refreshed by every
    /// live connection, and the only way to reach it on the local network only.
    public let addresses: [String]
    public init(_ v: JSONValue) {
        name = v["name"].string ?? ""; level = v["level_at_pairing"].string; path = v["path"].string
        relay = v["relay"].string.flatMap(Self.relayHost); relayN0 = v["relay_n0"] == .bool(true)
        p2pObstacle = v["p2p_obstacle"].string
        addresses = v["addresses"].array.compactMap(\.string)
    }

    /// Short name of the route, or nil while it is not known.
    public var routeLabel: String? {
        switch path {
        case "direct": "直连"
        case "relay": relayN0 ? "n0 公共中继" : "中继"
        default: nil
        }
    }

    /// While relayed: a one-line note that P2P failed and who forwards the traffic, and its explanation.
    public var relayNote: (note: String, detail: String)? {
        guard path == "relay" else { return nil }
        let relay = relay ?? "?"
        let why = switch p2pObstacle {
        case "udp_blocked": "\n可能的原因：这台设备所在网络不放行 UDP。"
        case "symmetric_nat": "\n可能的原因：这台设备所在网络是对称型 NAT，打洞很难成功。"
        case "captive_portal": "\n可能的原因：这台设备所在网络需要先在网页上登录。"
        case "remote": "\n这台设备的网络允许直连，原因多半在电脑那边的网络。"
        default: ""
        }
        return relayN0
            ? ("P2P 直连失败 · 经 n0 公共中继转发",
               "没能建立直连（P2P），流量正经 n0 的免费公共中继 \(relay) 转发。仍是端到端加密，但延迟更高，而且 n0 会限速。改用自建中继可以避开限速。" + why)
            : ("P2P 直连失败 · 经中继转发",
               "没能建立直连（P2P），流量正经中继 \(relay) 转发。仍是端到端加密，延迟会更高。" + why)
    }

    /// `https://host:port` → `host`, without the trailing root dot.
    static func relayHost(_ url: String) -> String? {
        let rest = url.range(of: "://").map { String(url[$0.upperBound...]) } ?? url
        var host = String(rest.split(whereSeparator: { $0 == "/" || $0 == ":" }).first ?? "")
        while host.hasSuffix(".") { host.removeLast() }
        return host.isEmpty ? nil : host
    }
}

public struct ScreenGeometry: Sendable, Equatable {
    public let x, y, width, height: Double
    public init?(_ v: JSONValue) {
        guard let x = v["x"].number, let y = v["y"].number, let w = v["width"].number, let h = v["height"].number,
              w > 0, h > 0, [x,y,w,h].allSatisfy({ $0.isFinite }) else { return nil }
        self.x = x; self.y = y; width = w; height = h
    }
    /// Touch coordinates in the displayed picture (not its letterboxing), mapped to host points.
    public func point(u: Double, v: Double) -> (Double, Double)? {
        guard (0...1).contains(u), (0...1).contains(v) else { return nil }
        return ((x + u * (width - 1)).rounded(), (y + v * (height - 1)).rounded())
    }
}

/// The codec of a video frame, from bits 1-2 of the header's flags.
public enum ScreenCodec: UInt8, Sendable {
    case h264 = 0, hevc = 1, av1 = 2
}

/// The 18-byte header every screen/1 message starts with (see the host's stream protocol).
public struct ScreenHeader: Sendable {
    public let kind: UInt8
    public let flags: UInt8
    public let width: Int
    public let height: Int
    public let microseconds: UInt64
    public let length: Int
    public init?(_ data: Data) {
        guard data.count >= 18 else { return nil }
        let b = [UInt8](data.prefix(18))
        kind = b[0]; flags = b[1]
        width = Int(b[2]) << 8 | Int(b[3]); height = Int(b[4]) << 8 | Int(b[5])
        microseconds = b[6..<14].reduce(UInt64(0)) { ($0 << 8) | UInt64($1) }
        length = b[14..<18].reduce(0) { ($0 << 8) | Int($1) }
        guard length > 0, length <= 8 * 1024 * 1024, data.count == 18 + length else { return nil }
    }
}

/// Where the host's pointer is (kind 4): its hot spot relative to the streamed screen rect.
public struct PointerPosition: Sendable, Equatable {
    public let x: Double
    public let y: Double
    public let visible: Bool
    /// The picture to draw (a kind-5 message with this id came before); 0: a default arrow.
    public let shape: UInt32
    public init?(_ data: Data) {
        guard let header = ScreenHeader(data), header.kind == 4,
              let json = try? JSONValue.parse(String(decoding: data.dropFirst(18), as: UTF8.self)),
              let x = json["x"].number, let y = json["y"].number, x.isFinite, y.isFinite else { return nil }
        self.x = x; self.y = y
        visible = json["visible"] != .bool(false)
        shape = UInt32(clamping: Int(json["shape"].number ?? 0))
    }
}

/// A pointer picture (kind 5): straight-alpha BGRA turned into a CGImage.
public struct PointerShape: Sendable {
    public let id: UInt32
    public let width: Int
    public let height: Int
    public let hotX: Int
    public let hotY: Int
    public let image: CGImage
    public init?(_ data: Data) {
        guard let header = ScreenHeader(data), header.kind == 5, header.width > 0, header.height > 0,
              header.width <= 512, header.height <= 512, header.length == 8 + header.width * header.height * 4 else { return nil }
        let b = [UInt8](data.dropFirst(18))
        id = UInt32(b[0]) << 24 | UInt32(b[1]) << 16 | UInt32(b[2]) << 8 | UInt32(b[3])
        hotX = Int(b[4]) << 8 | Int(b[5]); hotY = Int(b[6]) << 8 | Int(b[7])
        width = header.width; height = header.height
        let pixels = Data(b[8...])
        guard let provider = CGDataProvider(data: pixels as CFData),
              let image = CGImage(width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: width * 4,
                                  space: CGColorSpaceCreateDeviceRGB(),
                                  // B, G, R, A in memory: little-endian ARGB with straight alpha.
                                  bitmapInfo: CGBitmapInfo(rawValue: CGBitmapInfo.byteOrder32Little.rawValue | CGImageAlphaInfo.first.rawValue),
                                  provider: provider, decode: nil, shouldInterpolate: true, intent: .defaultIntent) else { return nil }
        self.image = image
    }
}

/// A video frame (kind 1): H.264 or HEVC Annex B.
public struct ScreenFrame: Sendable {
    public let width: Int
    public let height: Int
    public let keyframe: Bool
    public let codec: ScreenCodec
    public let microseconds: UInt64
    public let payload: Data
    public init?(_ data: Data) {
        guard let header = ScreenHeader(data), header.kind == 1, header.width > 0, header.height > 0,
              let codec = ScreenCodec(rawValue: (header.flags >> 1) & 3) else { return nil }
        width = header.width; height = header.height
        keyframe = header.flags & 1 == 1
        self.codec = codec
        microseconds = header.microseconds
        payload = Data(data.dropFirst(18))
    }
    public var nalUnits: [Data] {
        let b = [UInt8](payload)
        var starts: [(Int,Int)] = []
        var i = 0
        while i + 2 < b.count {
            if b[i] == 0 && b[i+1] == 0 {
                if b[i+2] == 1 { starts.append((i,i+3)); i += 3; continue }
                if i + 3 < b.count && b[i+2] == 0 && b[i+3] == 1 { starts.append((i,i+4)); i += 4; continue }
            }
            i += 1
        }
        return starts.enumerated().compactMap { index, start in
            let end = index + 1 < starts.count ? starts[index+1].0 : b.count
            return end > start.1 ? Data(b[start.1..<end]) : nil
        }
    }
}

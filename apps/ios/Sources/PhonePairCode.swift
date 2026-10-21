import Foundation

/// A pair code as people type it, or as a host's QR code carries it (`ai-remote://pair?code=48210937`).
/// The host shows eight digits for ten minutes; the controller agent finds the host behind them and
/// redeems its ticket, so the app only passes the digits on.
public enum PairCodeText {
    /// The eight digits of `text`, or nil when it is not a pair code (a ticket, another QR code).
    public static func digits(_ text: String) -> String? {
        var s = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if let url = URLComponents(string: s), url.scheme == "ai-remote" {
            guard url.host == "pair", let code = url.queryItems?.first(where: { $0.name == "code" })?.value else { return nil }
            s = code
        }
        guard s.count <= 40 else { return nil }
        var out = ""
        for scalar in s.unicodeScalars {
            switch scalar.value {
            case 0x30...0x39: out.unicodeScalars.append(scalar)
            // Full-width digits from a Chinese or Japanese keyboard.
            case 0xFF10...0xFF19: out.unicodeScalars.append(Unicode.Scalar(scalar.value - 0xFF10 + 0x30)!)
            case 0x20, 0x2D, 0x2E, 0x5F, 0xA0, 0x2009, 0x202F, 0x3000: continue
            default: return nil
            }
        }
        return out.count == 8 ? out : nil
    }

    /// What has been typed so far: digits only, at most eight, grouped `4821 0937`.
    public static func grouped(_ text: String) -> String {
        let d = String(text.unicodeScalars.filter { (0x30...0x39).contains($0.value) }.prefix(8).map(Character.init))
        return d.count > 4 ? "\(d.prefix(4)) \(d.dropFirst(4))" : d
    }

    /// Whether pasted text is the long ticket rather than a code.
    public static func isTicket(_ text: String) -> Bool {
        let s = text.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return s.hasPrefix("mcpmesh-invite:") || s.hasPrefix("mcpmesh-enroll:") || s.hasPrefix("ai-remote pair mcpmesh-")
    }
}

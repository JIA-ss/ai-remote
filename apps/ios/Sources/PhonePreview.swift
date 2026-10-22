import Foundation

// Previews of files a coding agent made or changed (HTML, Markdown, images), as pure functions:
// which paths can be previewed, which text in a reply names one, which files a finished turn shows,
// and the document an HTML preview renders. The phone version of the desktop's
// apps/desktop/src/lib/preview.ts; change both together (tools/PhoneChatModelTests.swift checks this
// one with the desktop's tools/chat-preview.test.mjs cases).
//
// The bytes come from the host's `chat_file` tool (crates/ar-host/src/chat/preview.rs) through
// `MobileController.previewFile`; the views are PhoneChatPreviewViews.swift.

public enum PhonePreviewKind: String, Sendable {
    case html, markdown, image
}

private let previewKinds: [String: PhonePreviewKind] = [
    "html": .html, "htm": .html, "md": .markdown, "markdown": .markdown,
    "png": .image, "jpg": .image, "jpeg": .image, "gif": .image, "webp": .image, "svg": .image, "bmp": .image,
]

/// Most text an HTML or Markdown preview renders; the rest is left out (the source view shows it).
public let phoneMaxRendered = 2 * 1024 * 1024
/// Most files shown inline under one turn.
public let phoneMaxInlinePreviews = 4

private func previewExtension(_ path: String) -> String {
    let name = phoneFileName(path)
    guard let dot = name.lastIndex(of: "."), dot > name.startIndex else { return "" }
    return name[name.index(after: dot)...].lowercased()
}

/// The last part of a path, with either separator.
public func phoneFileName(_ path: String) -> String {
    path.split(whereSeparator: { $0 == "/" || $0 == "\\" }).last.map(String.init) ?? path
}

/// What `path` is previewed as, or nil when it cannot be previewed.
public func phonePreviewKind(_ path: String) -> PhonePreviewKind? {
    previewKinds[previewExtension(path)]
}

/// Whether an image preview is SVG (drawn by a web view: the image decoders do not read it).
public func phonePreviewIsSVG(_ path: String) -> Bool { previewExtension(path) == "svg" }

private func matches(_ pattern: String, _ text: String, options: NSRegularExpression.Options = []) -> Bool {
    (try? NSRegularExpression(pattern: pattern, options: options))?
        .firstMatch(in: text, range: NSRange(text.startIndex..., in: text)) != nil
}

private func replacing(_ pattern: String, in text: String, with template: String = "") -> String {
    guard let re = try? NSRegularExpression(pattern: pattern) else { return text }
    return re.stringByReplacingMatches(in: text, range: NSRange(text.startIndex..., in: text), withTemplate: template)
}

/// The file a link or a bit of inline code in a reply names, when it names a previewable one:
/// `out/report.html`, `./notes.md`, `C:\work\chart.png`, `file:///tmp/a.svg`, `report.html#top`.
/// Web addresses, other schemes and text with spaces are not files.
public func phoneFileTarget(_ text: String) -> String? {
    var s = text.trimmingCharacters(in: .whitespacesAndNewlines)
    if s.isEmpty || s.count > 1024 || matches("\\s", s.replacingOccurrences(of: "%20", with: "")) { return nil }
    if matches("^file://", s, options: .caseInsensitive) {
        s = String(s.dropFirst("file://".count))
        // file:///C:/x on Windows
        if matches("^/[A-Za-z]:/", s) { s = String(s.dropFirst()) }
        guard let decoded = s.removingPercentEncoding else { return nil }
        s = decoded
    } else if matches("^[A-Za-z][A-Za-z0-9+.-]*:", s) && !matches("^[A-Za-z]:[\\\\/]", s) {
        // http:, mailto:, data:, ... (but not a Windows drive)
        return nil
    }
    s = replacing("[#?].*$", in: s)
    s = replacing(":\\d+(:\\d+)?$", in: s)
    if s.isEmpty || s.hasSuffix("/") || s.hasSuffix("\\") { return nil }
    return phonePreviewKind(s) == nil ? nil : s
}

/// The previewable files a finished turn made or named, in order: files its tools wrote (an edit's
/// diff, a location of a tool that is not a read or a delete), then files its last reply names (a
/// link or inline code). One per file name; failed tools and deleted files are left out.
public func phoneTurnFiles(_ blocks: [PhoneChatBlock], max: Int = phoneMaxInlinePreviews) -> [String] {
    var out: [String] = []
    var seen = Set<String>()
    func add(_ path: String?) {
        guard let path, phonePreviewKind(path) != nil else { return }
        let name = phoneFileName(path).lowercased()
        if seen.insert(name).inserted { out.append(path) }
    }
    for block in blocks {
        guard case .tool(let x) = block, x.status != "failed", x.kind != "read", x.kind != "delete" else { continue }
        for c in x.content {
            if case .diff(let path, let old, let new) = c, !(new.isEmpty && !(old ?? "").isEmpty) { add(path) }
        }
        for l in x.locations { add(l) }
    }
    let reply: String? = blocks.reversed().lazy.compactMap { if case .text(_, let t) = $0 { t } else { nil } }.first
    if let reply, let re = try? NSRegularExpression(pattern: "\\]\\(([^)\\s]+)\\)|`([^`\\s]+)`") {
        for m in re.matches(in: reply, range: NSRange(reply.startIndex..., in: reply)) {
            for group in 1...2 {
                if let r = Range(m.range(at: group), in: reply) { add(phoneFileTarget(String(reply[r]))) }
            }
        }
    }
    return Array(out.prefix(max))
}

/// The document an HTML preview renders. A policy at the top of its head says nothing is fetched
/// (images only from `data:` and `blob:`, styles inline) and nothing is submitted; the web view also
/// runs no script and blocks every load (PhoneChatPreviewViews.swift), so this is the second wall.
/// A policy of the page's own comes after and can only narrow this one. A viewport for the phone's
/// width follows the policy (one of the page's own, later, wins).
public func phonePreviewDocument(_ source: String) -> String {
    let guardTags = "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data: blob:; "
        + "style-src 'unsafe-inline'; font-src data:; media-src data: blob:; base-uri 'none'; form-action 'none'\">"
        // Phone width, as a page made for a desktop browser would otherwise be drawn tiny.
        + "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">"
    let ns = source as NSString
    let whole = NSRange(location: 0, length: ns.length)
    if let head = try? NSRegularExpression(pattern: "<head\\b[^>]*>", options: .caseInsensitive),
       let m = head.firstMatch(in: source, range: whole) {
        let end = m.range.location + m.range.length
        return ns.substring(to: end) + guardTags + ns.substring(from: end)
    }
    if let doctype = try? NSRegularExpression(pattern: "^\\s*<!doctype[^>]*>", options: .caseInsensitive),
       let m = doctype.firstMatch(in: source, range: whole) {
        let end = m.range.location + m.range.length
        return ns.substring(to: end) + guardTags + ns.substring(from: end)
    }
    return guardTags + source
}

/// Text of a file, cut to `max` bytes' worth (on a character boundary) when longer.
public func phoneDecodeText(_ bytes: Data, max: Int = phoneMaxRendered) -> (text: String, cut: Bool) {
    let cut = bytes.count > max
    var slice = cut ? bytes.prefix(max) : bytes
    if cut {
        // Drop a character the cut split (at most three trailing bytes of a UTF-8 sequence).
        for _ in 0..<4 where String(data: slice, encoding: .utf8) == nil && !slice.isEmpty { slice = slice.dropLast() }
    }
    return (String(decoding: slice, as: UTF8.self), cut)
}

/// "12 KB", "3.4 MB".
public func phoneSizeLabel(_ n: Int) -> String {
    if n < 1024 { return "\(n) B" }
    if n < 1024 * 1024 { return "\(Int((Double(n) / 1024).rounded())) KB" }
    return String(format: "%.1f MB", Double(n) / 1_048_576)
}

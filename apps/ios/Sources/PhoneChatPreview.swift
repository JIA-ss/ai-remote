import SwiftUI
#if canImport(WebKit)
import WebKit
#endif

// Previews of the HTML, Markdown and image files a coding agent made or changed. Under each finished
// turn the files it wrote or named show as cards (an image as a thumbnail); a tap, the eye button on
// an edit card, or a file named by a link in a reply opens the file in a sheet. The bytes come from
// the host (`chat_file`: only files the session is about, at most 16 MiB) into memory, once per
// conversation. The phone version of the desktop's apps/desktop/src/ui/chat/Preview.tsx.
//
// HTML is drawn by a WKWebView with JavaScript turned off for its content, a policy at the top of the
// page that fetches nothing from the network (`phonePreviewDocument`), a non-persistent data store,
// and every navigation after the first load refused. Static HTML and CSS still render; pages that
// need scripts (chart libraries) show only their static parts, and the source view shows the rest.
// Not running agent code at all is the desktop's rule as well; on the phone a web view could not
// reach the app's own code (it has no script message handlers), so this is caution rather than a
// hole being closed, at the cost of script-built pages.

/// Opens the preview of a file this conversation is about (nil outside a conversation).
struct PhonePreviewOpenKey: EnvironmentKey {
    static let defaultValue: ((String) -> Void)? = nil
}

extension EnvironmentValues {
    var phonePreviewOpen: ((String) -> Void)? {
        get { self[PhonePreviewOpenKey.self] }
        set { self[PhonePreviewOpenKey.self] = newValue }
    }
}

private func kindIcon(_ kind: String?) -> String {
    switch kind {
    case "html"?: "globe"
    case "markdown"?: "doc.richtext"
    default: "photo"
    }
}

private func kindName(_ kind: String?) -> String {
    switch kind {
    case "html"?: "网页"
    case "markdown"?: "Markdown"
    default: "图片"
    }
}

/// An eye button that previews `path`, when the conversation can and the file is a kind it shows.
struct PhonePreviewButton: View {
    let path: String
    @Environment(\.phonePreviewOpen) private var open
    var body: some View {
        if let open, phonePreviewKind(path) != nil {
            Button { open(path) } label: { Image(systemName: "eye").font(.caption) }
                .buttonStyle(.plain).foregroundStyle(.secondary)
                .accessibilityLabel("预览 \(phoneBaseName(path))")
                .accessibilityIdentifier("chat-preview-open-\(phoneBaseName(path))")
        }
    }
}

/// The files a finished turn wrote or named, as cards under it.
struct PhoneTurnPreviews: View {
    let chat: PhoneChatStore
    let files: [String]
    @Environment(\.phonePreviewOpen) private var open
    var body: some View {
        if !files.isEmpty {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    ForEach(files, id: \.self) { path in
                        Button { open?(path) } label: { PhonePreviewCard(chat: chat, path: path) }
                            .buttonStyle(.plain)
                            .accessibilityIdentifier("chat-preview-\(phoneBaseName(path))")
                    }
                }
            }
        }
    }
}

/// One file under a turn: an image's thumbnail, or the file's kind and name.
private struct PhonePreviewCard: View {
    let chat: PhoneChatStore
    let path: String
    @PhoneViewState private var thumb: CGImage?
    @PhoneViewState private var failed = false
    var body: some View {
        let kind = phonePreviewKind(path)
        VStack(alignment: .leading, spacing: 6) {
            if kind == "image", let thumb {
                Image(decorative: thumb, scale: 1).resizable().scaledToFill()
                    .frame(width: 132, height: 84).clipped()
                    .clipShape(RoundedRectangle(cornerRadius: 6))
            } else {
                Image(systemName: kindIcon(kind)).font(.title3).foregroundStyle(.secondary)
                    .frame(width: 132, height: kind == "image" ? 84 : 36, alignment: kind == "image" ? .center : .leading)
            }
            Text(phoneBaseName(path)).font(.caption.weight(.medium)).lineLimit(1).truncationMode(.middle)
            Text(failed ? "打不开" : kindName(kind)).font(.caption2).foregroundStyle(failed ? .orange : .secondary)
        }
        .frame(width: 132, alignment: .leading)
        .padding(8)
        .background(Color.secondary.opacity(0.07), in: RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(Color.secondary.opacity(0.15)))
        .task(id: path) {
            guard kind == "image", thumb == nil else { return }
            do {
                let data = try await chat.preview(path)
                thumb = phoneDecodeImage(data, maxPixels: 400)
            } catch { failed = true }
        }
    }
}

// MARK: - The sheet

struct PhonePreviewSheet: View {
    let chat: PhoneChatStore
    let path: String
    let close: () -> Void
    private enum Loaded { case loading, failed(String), ready(Data) }
    @PhoneViewState private var loaded: Loaded = .loading
    @PhoneViewState private var source = false
    @PhoneViewState private var round = 0

    var body: some View {
        let kind = phonePreviewKind(path)
        NavigationStack {
            content(kind)
                .navigationTitle(phoneBaseName(path))
                #if os(iOS)
                .navigationBarTitleDisplayMode(.inline)
                #endif
                .toolbar {
                    ToolbarItem(placement: .confirmationAction) { Button("完成", action: close).accessibilityIdentifier("chat-preview-done") }
                    if kind != "image" {
                        ToolbarItem(placement: .principal) {
                            Picker("显示", selection: $source) {
                                Text("预览").tag(false)
                                Text("源码").tag(true)
                            }
                            .pickerStyle(.segmented).frame(width: 140)
                            .accessibilityIdentifier("chat-preview-mode")
                        }
                    }
                    ToolbarItem(placement: .cancellationAction) {
                        Button { round += 1 } label: { Image(systemName: "arrow.clockwise") }.accessibilityLabel("重新获取")
                    }
                }
                .safeAreaInset(edge: .bottom) {
                    Text(path).font(.caption2.monospaced()).foregroundStyle(.secondary).lineLimit(2).truncationMode(.head)
                        .frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal).padding(.vertical, 6).background(.bar)
                }
        }
        .task(id: round) {
            loaded = .loading
            do { loaded = .ready(try await chat.preview(path, fresh: round > 0)) } catch { loaded = .failed(error.localizedDescription) }
        }
        .accessibilityIdentifier("chat-preview-sheet")
    }

    @ViewBuilder private func content(_ kind: String?) -> some View {
        switch loaded {
        case .loading:
            ProgressView("正在从电脑上取文件…").frame(maxWidth: .infinity, maxHeight: .infinity)
        case .failed(let why):
            VStack(spacing: 10) {
                Image(systemName: "exclamationmark.triangle").font(.largeTitle).foregroundStyle(.orange)
                Text(why).font(.footnote).foregroundStyle(.secondary).multilineTextAlignment(.center)
            }
            .padding().frame(maxWidth: .infinity, maxHeight: .infinity)
            .accessibilityIdentifier("chat-preview-error")
        case .ready(let data):
            ready(kind, data)
        }
    }

    @ViewBuilder private func ready(_ kind: String?, _ data: Data) -> some View {
        let text = String(decoding: data.prefix(phoneMaxRendered), as: UTF8.self)
        if kind == "image" {
            if let image = phoneDecodeImage(data) {
                PhoneZoomImage(image: image)
            } else {
                // SVG (or a format ImageIO cannot read): the web view draws it, still without scripts.
                PhoneHTMLPreview(html: "<!doctype html><html><head><style>body{margin:0;display:flex;justify-content:center}img{max-width:100%}</style></head>"
                                 + "<body><img src=\"data:\(phoneImageMime(path));base64,\(data.base64EncodedString())\"></body></html>")
            }
        } else if source {
            ScrollView([.vertical, .horizontal]) {
                Text(text).font(.caption.monospaced()).textSelection(.enabled).padding()
            }
            .accessibilityIdentifier("chat-preview-source")
        } else if kind == "markdown" {
            ScrollView {
                PhoneMarkdownDocument(text: text).padding()
            }
            .accessibilityIdentifier("chat-preview-markdown")
        } else {
            PhoneHTMLPreview(html: phonePreviewDocument(text)).accessibilityIdentifier("chat-preview-html")
        }
    }
}

/// An image that fits the sheet; a double tap shows it at its own size (and back), pinching zooms.
private struct PhoneZoomImage: View {
    let image: CGImage
    @PhoneViewState private var actual = false
    @PhoneViewState private var zoom: CGFloat = 1
    var body: some View {
        GeometryReader { geo in
            ScrollView([.horizontal, .vertical]) {
                Image(decorative: image, scale: 1).resizable().interpolation(.high)
                    .aspectRatio(contentMode: .fit)
                    .frame(width: actual ? CGFloat(image.width) / displayScale : geo.size.width * zoom)
                    .frame(minWidth: geo.size.width, minHeight: geo.size.height)
                    .onTapGesture(count: 2) { actual.toggle(); zoom = 1 }
                    .gesture(MagnificationGesture().onChanged { zoom = max(1, min(6, $0)) })
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("图片 \(image.width)×\(image.height)")
        .accessibilityIdentifier("chat-preview-image")
    }
    private var displayScale: CGFloat {
        #if os(iOS)
        UIScreen.main.scale
        #else
        2
        #endif
    }
}

#if os(iOS)
/// A page drawn without scripts, without network and without navigating anywhere.
struct PhoneHTMLPreview: UIViewRepresentable {
    let html: String
    func makeUIView(context: Context) -> WKWebView {
        let config = WKWebViewConfiguration()
        config.websiteDataStore = .nonPersistent()
        config.defaultWebpagePreferences.allowsContentJavaScript = false
        config.preferences.javaScriptCanOpenWindowsAutomatically = false
        let view = WKWebView(frame: .zero, configuration: config)
        view.navigationDelegate = context.coordinator
        view.isOpaque = false
        view.accessibilityIdentifier = "chat-preview-web"
        view.loadHTMLString(html, baseURL: nil)
        return view
    }
    func updateUIView(_ view: WKWebView, context: Context) {}
    func makeCoordinator() -> Coordinator { Coordinator() }
    final class Coordinator: NSObject, WKNavigationDelegate {
        private var loaded = false
        func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction, decisionHandler: @escaping @MainActor @Sendable (WKNavigationActionPolicy) -> Void) {
            // The page itself (about:blank with our document), and nothing after it: links go nowhere.
            if !loaded, action.navigationType == .other, action.request.url?.absoluteString == "about:blank" { decisionHandler(.allow); return }
            decisionHandler(.cancel)
        }
        func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) { loaded = true }
    }
}
#else
struct PhoneHTMLPreview: View {
    let html: String
    var body: some View { ScrollView { Text(html).font(.caption.monospaced()).padding() } }
}
#endif

// MARK: - Markdown as a document

/// A Markdown file: headings, paragraphs, lists, quotes, fenced code, rules and tables, with inline
/// Markdown inside (AttributedString, which never renders HTML).
struct PhoneMarkdownDocument: View {
    let text: String
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(Self.blocks(text).enumerated()), id: \.offset) { _, block in view(block) }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .textSelection(.enabled)
    }

    enum Block: Equatable {
        case heading(Int, String), paragraph(String), bullet(String, ordered: String?, depth: Int), quote(String), code(String), rule, table([[String]])
    }

    @ViewBuilder private func view(_ b: Block) -> some View {
        switch b {
        case .heading(let level, let t):
            Text(PhoneMarkdownText.inline(t)).font(level == 1 ? .title2.bold() : level == 2 ? .title3.bold() : .headline)
                .padding(.top, level <= 2 ? 6 : 2)
        case .paragraph(let t): Text(PhoneMarkdownText.inline(t))
        case .bullet(let t, let ordered, let depth):
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(ordered ?? "•").foregroundStyle(.secondary)
                Text(PhoneMarkdownText.inline(t))
            }
            .padding(.leading, CGFloat(depth) * 16)
        case .quote(let t):
            Text(PhoneMarkdownText.inline(t)).foregroundStyle(.secondary).padding(.leading, 10)
                .overlay(alignment: .leading) { Rectangle().fill(Color.secondary.opacity(0.35)).frame(width: 3) }
        case .code(let t):
            ScrollView(.horizontal, showsIndicators: false) { Text(t).font(.footnote.monospaced()).padding(10) }
                .background(Color.secondary.opacity(0.1), in: RoundedRectangle(cornerRadius: 8))
        case .rule: Divider()
        case .table(let rows):
            ScrollView(.horizontal, showsIndicators: false) {
                Grid(alignment: .leading, horizontalSpacing: 14, verticalSpacing: 6) {
                    ForEach(Array(rows.enumerated()), id: \.offset) { i, row in
                        GridRow {
                            ForEach(Array(row.enumerated()), id: \.offset) { _, cell in
                                Text(PhoneMarkdownText.inline(cell)).font(i == 0 ? .subheadline.bold() : .subheadline)
                            }
                        }
                        if i == 0 { Divider() }
                    }
                }
                .padding(10)
            }
            .background(Color.secondary.opacity(0.06), in: RoundedRectangle(cornerRadius: 8))
        }
    }

    /// The blocks of a Markdown text, in order.
    static func blocks(_ text: String) -> [Block] {
        var out: [Block] = []
        var para: [String] = [], quote: [String] = [], code: [String] = [], table: [[String]] = []
        var inCode = false
        func flush() {
            if !para.isEmpty { out.append(.paragraph(para.joined(separator: " "))); para = [] }
            if !quote.isEmpty { out.append(.quote(quote.joined(separator: "\n"))); quote = [] }
            if !table.isEmpty { out.append(.table(table)); table = [] }
        }
        for raw in text.components(separatedBy: "\n") {
            let line = raw.trimmingCharacters(in: .whitespaces)
            if line.hasPrefix("```") || line.hasPrefix("~~~") {
                if inCode { out.append(.code(code.joined(separator: "\n"))); code = [] } else { flush() }
                inCode.toggle(); continue
            }
            if inCode { code.append(raw); continue }
            if line.isEmpty { flush(); continue }
            if let hashes = line.firstIndex(where: { $0 != "#" }), line.hasPrefix("#"), line[hashes] == " " {
                flush(); out.append(.heading(line.distance(from: line.startIndex, to: hashes), String(line[hashes...]).trimmingCharacters(in: .whitespaces))); continue
            }
            if line == "---" || line == "***" || line == "___" { flush(); out.append(.rule); continue }
            if line.hasPrefix(">") { if !para.isEmpty || !table.isEmpty { flush() }; quote.append(String(line.dropFirst()).trimmingCharacters(in: .whitespaces)); continue }
            if line.hasPrefix("|") {
                if !para.isEmpty || !quote.isEmpty { flush() }
                let cells = line.trimmingCharacters(in: CharacterSet(charactersIn: "|")).components(separatedBy: "|").map { $0.trimmingCharacters(in: .whitespaces) }
                // The row under the header (`|---|:--:|`) only aligns the columns.
                if !cells.allSatisfy({ !$0.isEmpty && $0.allSatisfy { "-:".contains($0) } }) { table.append(cells) }
                continue
            }
            let depth = (raw.prefix(while: { $0 == " " }).count) / 2
            if let marker = ["- ", "* ", "+ "].first(where: { line.hasPrefix($0) }) {
                flush()
                var item = String(line.dropFirst(marker.count))
                if item.hasPrefix("[ ] ") { item = "☐ " + item.dropFirst(4) } else if item.lowercased().hasPrefix("[x] ") { item = "☑ " + item.dropFirst(4) }
                out.append(.bullet(item, ordered: nil, depth: depth)); continue
            }
            if let dot = line.firstIndex(where: { $0 == "." || $0 == ")" }), dot != line.startIndex, line[..<dot].allSatisfy(\.isNumber),
               line.index(after: dot) < line.endIndex, line[line.index(after: dot)] == " " {
                flush(); out.append(.bullet(String(line[line.index(dot, offsetBy: 2)...]), ordered: String(line[...dot]), depth: depth)); continue
            }
            if !quote.isEmpty || !table.isEmpty { flush() }
            para.append(line)
        }
        if inCode { out.append(.code(code.joined(separator: "\n"))) }
        flush()
        return out
    }
}

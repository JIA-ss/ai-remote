import SwiftUI
import WebKit
#if os(iOS)
import UIKit
#elseif os(macOS)
import AppKit
#endif

// File previews in a conversation, the phone version of the desktop's apps/desktop/src/ui/chat/Preview.tsx
// (which follows Orca's inline visuals): under a finished turn, the HTML pages, Markdown and images its
// tools wrote or its reply names (phoneTurnFiles) as small cards, fetched when they come near the
// screen; tapping one opens it full screen, with its source for HTML and Markdown.
//
// HTML is drawn by a web view that runs no script (`allowsContentJavaScript = false`), loads nothing
// (a content rule list blocks every URL but about:, data: and blob:, and the navigation delegate allows
// only about: documents, so links do nothing), keeps no data (a non-persistent store) and has no
// message handlers or user scripts. The page also gets a `default-src 'none'` policy at the top of its
// head (phonePreviewDocument). Markdown is drawn like chat messages (PhoneMarkdownText), never as HTML.

public struct PhonePreviewTarget: Identifiable, Hashable, Sendable {
    public let path: String
    public var id: String { path }
    public init(path: String) { self.path = path }
}

/// The previews under a finished turn.
struct PhoneTurnPreviews: View {
    let turn: PhoneChatTurn
    let chat: PhoneChatStore
    var body: some View {
        let files = turn.status == "running" ? [] : phoneTurnFiles(turn.blocks)
        if !files.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(files, id: \.self) { path in PhonePreviewCard(path: path, chat: chat) }
            }
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("chat-previews-\(turn.n)")
        }
    }
}

private enum PreviewPhase {
    case loading
    case loaded(Data)
    case failed(String)
}

private func previewIcon(_ kind: PhonePreviewKind?) -> String {
    switch kind {
    case .html: "globe"
    case .markdown: "doc.richtext"
    case .image: "photo"
    case nil: "doc"
    }
}

/// One file under a turn: a small window onto it and its name; tap to open it full screen.
struct PhonePreviewCard: View {
    let path: String
    let chat: PhoneChatStore
    @PhoneViewState private var phase: PreviewPhase = .loading

    private var kind: PhonePreviewKind? { phonePreviewKind(path) }
    private var height: CGFloat {
        switch kind {
        case .html: 220
        case .markdown: 150
        default: 96
        }
    }

    var body: some View {
        Button { chat.previewing = PhonePreviewTarget(path: path) } label: {
            VStack(alignment: .leading, spacing: 0) {
                window.frame(maxWidth: .infinity, alignment: .topLeading).frame(height: height).clipped()
                    .allowsHitTesting(false)
                Divider()
                HStack(spacing: 6) {
                    Image(systemName: previewIcon(kind)).foregroundStyle(.secondary)
                    Text(phoneFileName(path)).lineLimit(1).truncationMode(.middle)
                    Spacer(minLength: 4)
                    if case .loaded(let data) = phase { Text(phoneSizeLabel(data.count)).foregroundStyle(.secondary) }
                    Image(systemName: "arrow.up.left.and.arrow.down.right").foregroundStyle(.tertiary)
                }
                .font(.caption)
                .padding(.horizontal, 10).padding(.vertical, 7)
            }
            .background(Color.secondary.opacity(0.06))
            .clipShape(RoundedRectangle(cornerRadius: 10))
            .overlay(RoundedRectangle(cornerRadius: 10).stroke(Color.secondary.opacity(0.2)))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("预览 \(phoneFileName(path))")
        .accessibilityIdentifier("chat-preview-\(phoneFileName(path))")
        // In a lazy stack: fetched when the card comes near the screen.
        .task(id: path) {
            do { phase = .loaded(try await chat.preview(path)) }
            catch { phase = .failed(error.localizedDescription) }
        }
    }

    @ViewBuilder private var window: some View {
        switch phase {
        case .loading:
            ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
        case .failed(let reason):
            Text(reason).font(.caption).foregroundStyle(.secondary).lineLimit(4).padding(10)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        case .loaded(let data):
            PhonePreviewBody(path: path, data: data, inline: true)
        }
    }
}

/// A file drawn as itself: an HTML page, Markdown, an image (SVG through the locked web view).
struct PhonePreviewBody: View {
    let path: String
    let data: Data
    let inline: Bool
    @PhoneViewState private var actualSize = false

    var body: some View {
        switch phonePreviewKind(path) {
        case .html:
            PhonePreviewWeb(html: phonePreviewDocument(phoneDecodeText(data).text))
        case .markdown:
            let text = phoneDecodeText(data, max: inline ? 8 * 1024 : phoneMaxRendered)
            if inline {
                PhoneMarkdownText(text: text.text, font: .footnote).padding(10)
                    .mask(LinearGradient(stops: [.init(color: .black, location: 0.6), .init(color: .clear, location: 1)],
                                         startPoint: .top, endPoint: .bottom))
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        PhoneMarkdownText(text: text.text)
                        if text.cut { Text("文件太长，只显示了开头。").font(.footnote).foregroundStyle(.secondary) }
                    }
                    .padding(16)
                }
            }
        case .image where phonePreviewIsSVG(path):
            PhonePreviewWeb(html: phonePreviewDocument(
                "<!doctype html><html><head><meta name=\"viewport\" content=\"width=device-width\"></head>"
                    + "<body style=\"margin:0;display:flex;align-items:\(inline ? "flex-start" : "center");justify-content:\(inline ? "flex-start" : "center");min-height:100vh\">"
                    + "<img style=\"max-width:100%;max-height:100vh\" src=\"data:image/svg+xml;base64,\(data.base64EncodedString())\"></body></html>"))
        case .image:
            if let image = PhonePlatformImage(data: data) {
                if inline {
                    picture(image).resizable().scaledToFit().frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
                        .padding(6)
                } else {
                    GeometryReader { box in
                        ScrollView([.horizontal, .vertical]) {
                            Group {
                                if actualSize { picture(image) } else { picture(image).resizable().scaledToFit() }
                            }
                            .frame(minWidth: box.size.width, minHeight: box.size.height)
                        }
                        .onTapGesture { actualSize.toggle() }
                    }
                }
            } else {
                Text("无法显示这张图片。").font(.caption).foregroundStyle(.secondary).padding(10)
            }
        case nil:
            Text("不能预览这种文件。").font(.caption).foregroundStyle(.secondary).padding(10)
        }
    }

    private func picture(_ image: PhonePlatformImage) -> Image {
        #if os(iOS)
        Image(uiImage: image)
        #else
        Image(nsImage: image)
        #endif
    }
}

#if os(iOS)
typealias PhonePlatformImage = UIImage
#else
typealias PhonePlatformImage = NSImage
#endif

/// A file full screen: drawn, or its source (HTML, Markdown); failures say why and can fetch again.
struct PhonePreviewScreen: View {
    let target: PhonePreviewTarget
    let chat: PhoneChatStore
    @PhoneViewState private var phase: PreviewPhase = .loading
    @PhoneViewState private var source = false
    @Environment(\.dismiss) private var dismiss

    private var kind: PhonePreviewKind? { phonePreviewKind(target.path) }

    var body: some View {
        NavigationStack {
            content
                .navigationTitle(phoneFileName(target.path))
                #if os(iOS)
                .navigationBarTitleDisplayMode(.inline)
                #endif
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("关闭") { dismiss() }.accessibilityIdentifier("chat-preview-close")
                    }
                    if kind == .html || kind == .markdown {
                        ToolbarItem(placement: .primaryAction) {
                            Picker("显示", selection: $source) {
                                Text("预览").tag(false)
                                Text("源码").tag(true)
                            }
                            .pickerStyle(.segmented)
                            .accessibilityIdentifier("chat-preview-mode")
                        }
                    }
                }
        }
        .task { await load(again: false) }
    }

    @ViewBuilder private var content: some View {
        switch phase {
        case .loading:
            ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
        case .failed(let reason):
            VStack(spacing: 12) {
                Image(systemName: "exclamationmark.triangle").font(.largeTitle).foregroundStyle(.secondary)
                Text(reason).font(.footnote).foregroundStyle(.secondary).multilineTextAlignment(.center)
                Button("重新获取") { Task { await load(again: true) } }.buttonStyle(.bordered)
            }
            .padding(24).frame(maxWidth: .infinity, maxHeight: .infinity)
        case .loaded(let data):
            if source {
                let text = phoneDecodeText(data, max: 512 * 1024)
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        Text(text.text).font(.footnote.monospaced()).textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                        if text.cut { Text("源码太长，只显示了前 512 KB。").font(.footnote).foregroundStyle(.secondary) }
                    }
                    .padding(16)
                }
                .accessibilityIdentifier("chat-preview-source")
            } else {
                PhonePreviewBody(path: target.path, data: data, inline: false)
                    .accessibilityIdentifier("chat-preview-body")
            }
        }
    }

    private func load(again: Bool) async {
        phase = .loading
        do { phase = .loaded(try await chat.preview(target.path, again: again)) }
        catch { phase = .failed(error.localizedDescription) }
    }
}

// MARK: - The locked web view

/// Every URL is blocked but the page itself (about:) and what it carries inline (data:, blob:).
/// WebKit's URL filters have no alternation, so the exceptions are separate rules.
private let blockEverything = """
[{"trigger":{"url-filter":".*"},"action":{"type":"block"}},
 {"trigger":{"url-filter":"^about:"},"action":{"type":"ignore-previous-rules"}},
 {"trigger":{"url-filter":"^data:"},"action":{"type":"ignore-previous-rules"}},
 {"trigger":{"url-filter":"^blob:"},"action":{"type":"ignore-previous-rules"}}]
"""

@MainActor enum PhonePreviewBlocker {
    private static var compiled: WKContentRuleList?
    private static var failure: String?
    /// The compiled rule list, or why it could not be made (then no page is shown at all).
    static func list() async -> Result<WKContentRuleList, Error> {
        if let compiled { return .success(compiled) }
        do {
            guard let list = try await WKContentRuleListStore.default()
                .compileContentRuleList(forIdentifier: "ai-remote-preview-block-all", encodedContentRuleList: blockEverything)
            else { throw NSError(domain: "ai-remote", code: 6, userInfo: [NSLocalizedDescriptionKey: "无法建立预览的网络拦截规则"]) }
            compiled = list
            return .success(list)
        } catch { return .failure(error) }
    }
}

/// An HTML document in the locked web view; nothing is shown until the blocking rules are in place.
struct PhonePreviewWeb: View {
    let html: String
    @PhoneViewState private var blocker: Result<WKContentRuleList, Error>?

    var body: some View {
        Group {
            switch blocker {
            case .success(let list)?: PhonePreviewWebView(html: html, blocker: list)
            case .failure(let error)?:
                Text(error.localizedDescription).font(.caption).foregroundStyle(.secondary).padding(10)
            case nil: ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .task { if blocker == nil { blocker = await PhonePreviewBlocker.list() } }
    }
}

/// Allows the page itself (about:blank, about:srcdoc) and nothing else: links, frames and redirects
/// to anywhere are cancelled.
@MainActor final class PhonePreviewNavigation: NSObject, WKNavigationDelegate {
    var loaded: String?
    func webView(_ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction) async -> WKNavigationActionPolicy {
        navigationAction.request.url?.scheme?.lowercased() == "about" ? .allow : .cancel
    }
}

@MainActor private func lockedWebView(_ blocker: WKContentRuleList, _ navigation: PhonePreviewNavigation) -> WKWebView {
    let config = WKWebViewConfiguration()
    config.defaultWebpagePreferences.allowsContentJavaScript = false
    config.preferences.javaScriptCanOpenWindowsAutomatically = false
    config.websiteDataStore = .nonPersistent()
    // The only thing added to the content controller: no message handlers, no user scripts.
    config.userContentController.add(blocker)
    config.mediaTypesRequiringUserActionForPlayback = .all
    #if os(iOS)
    config.dataDetectorTypes = []
    #endif
    let web = WKWebView(frame: .zero, configuration: config)
    web.navigationDelegate = navigation
    web.allowsLinkPreview = false
    return web
}

#if os(iOS)
struct PhonePreviewWebView: UIViewRepresentable {
    let html: String
    let blocker: WKContentRuleList
    func makeCoordinator() -> PhonePreviewNavigation { PhonePreviewNavigation() }
    func makeUIView(context: Context) -> WKWebView {
        let web = lockedWebView(blocker, context.coordinator)
        web.accessibilityIdentifier = "chat-preview-web"
        return web
    }
    func updateUIView(_ web: WKWebView, context: Context) {
        guard context.coordinator.loaded != html else { return }
        context.coordinator.loaded = html
        web.loadHTMLString(html, baseURL: nil)
    }
}
#else
struct PhonePreviewWebView: NSViewRepresentable {
    let html: String
    let blocker: WKContentRuleList
    func makeCoordinator() -> PhonePreviewNavigation { PhonePreviewNavigation() }
    func makeNSView(context: Context) -> WKWebView { lockedWebView(blocker, context.coordinator) }
    func updateNSView(_ web: WKWebView, context: Context) {
        guard context.coordinator.loaded != html else { return }
        context.coordinator.loaded = html
        web.loadHTMLString(html, baseURL: nil)
    }
}
#endif

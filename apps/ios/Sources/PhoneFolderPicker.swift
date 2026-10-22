import SwiftUI

// The folder an agent works in: recent folders first (this phone's, then those of the host's sessions),
// a search over them or a typed path, and browsing the host's folders through `files_list` (admin level
// and read-only, like `chat_start`, so a device that may start an agent may look). The phone version
// of the desktop's apps/desktop/src/ui/chat/FolderPicker.tsx, after Codex's project picker and
// Orca's project list.

/// How a host folder is shown: its name, and above it the path (the home as `~`).
struct PhoneFolderLabel: View {
    let path: String
    let home: String?
    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(phoneFolderName(path)).font(.body).lineLimit(1)
            if let parent = phoneParentFolder(phoneTildeFolder(path, home: home)) {
                Text(parent).font(.caption.monospaced()).foregroundStyle(.secondary).lineLimit(1).truncationMode(.head)
            }
        }
    }
}

struct PhoneFolderPicker: View {
    let chat: PhoneChatStore
    @Binding var selection: String
    @Environment(\.dismiss) private var dismiss
    @PhoneViewState private var query = ""
    /// The folder being browsed (nil: the recent list).
    @PhoneViewState private var browsing: String?
    @PhoneViewState private var folders: [String] = []
    @PhoneViewState private var more = false
    @PhoneViewState private var failure: String?
    @PhoneViewState private var loading = false

    var body: some View {
        List {
            if let dir = browsing { browseSections(dir) } else { recentSections }
        }
        .navigationTitle(browsing == nil ? "选择文件夹" : phoneFolderName(browsing ?? ""))
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        #endif
        .searchable(text: $query, prompt: browsing == nil ? "搜索最近的文件夹，或输入路径" : "筛选这里的文件夹")
        .onSubmit(of: .search) { if phoneLooksLikePath(query) { open(query) } }
        .safeAreaInset(edge: .bottom) {
            if let dir = browsing, failure == nil {
                Button { choose(dir) } label: {
                    Text("使用「\(phoneFolderName(dir))」").frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent).padding()
                .accessibilityIdentifier("chat-folder-use-bottom")
            }
        }
        .task { if chat.folderChoices.isEmpty { open("~") } }
        .accessibilityIdentifier("chat-folder-picker")
    }

    @ViewBuilder private var recentSections: some View {
        let choices = phoneMatchFolders(chat.folderChoices, phoneLooksLikePath(query) ? "" : query)
        if phoneLooksLikePath(query) {
            Section {
                Button { open(query) } label: { Label("打开 \(phoneTrimFolder(query))", systemImage: "arrow.right.circle") }
                    .accessibilityIdentifier("chat-folder-open-typed")
            }
        }
        if !choices.isEmpty {
            Section("最近") {
                ForEach(choices, id: \.self) { f in
                    Button { choose(f) } label: {
                        HStack {
                            Image(systemName: "folder").foregroundStyle(.secondary)
                            PhoneFolderLabel(path: f, home: chat.home)
                            Spacer()
                            if phoneSameFolder(f, selection) { Image(systemName: "checkmark").foregroundStyle(.tint) }
                        }
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("chat-folder-recent-\(phoneFolderName(f))")
                }
            }
        }
        Section {
            Button { open(phoneLooksLikePath(selection) ? selection : "~") } label: {
                Label("浏览这台电脑上的文件夹…", systemImage: "folder.badge.gearshape")
            }
            .accessibilityIdentifier("chat-folder-browse")
        }
    }

    @ViewBuilder private func browseSections(_ dir: String) -> some View {
        Section {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 4) {
                    ForEach(Array(phoneFolderCrumbs(phoneTildeFolder(dir, home: chat.home)).enumerated()), id: \.offset) { i, crumb in
                        if i > 0 { Image(systemName: "chevron.right").font(.caption2).foregroundStyle(.tertiary) }
                        Button(crumb.name) { open(crumb.path) }.font(.footnote).buttonStyle(.borderless)
                    }
                }
            }
            .accessibilityIdentifier("chat-folder-crumbs")
            Button { browsing = nil; query = "" } label: { Label("最近的文件夹", systemImage: "clock") }.font(.footnote)
        }
        if let failure {
            Section {
                Label(failure, systemImage: "exclamationmark.triangle").foregroundStyle(.orange).font(.footnote)
                    .accessibilityIdentifier("chat-folder-error")
                Button("仍然使用这个路径") { choose(dir) }.accessibilityIdentifier("chat-folder-use-anyway")
            } footer: { Text("启动时被控端还会再检查一次这个文件夹。") }
        } else if loading {
            Section { ProgressView().frame(maxWidth: .infinity) }
        } else {
            let shown = query.isEmpty ? folders.filter { !$0.hasPrefix(".") } : folders.filter { (query.hasPrefix(".") || !$0.hasPrefix(".")) && $0.lowercased().contains(query.lowercased()) }
            Section {
                if let up = phoneParentFolder(dir) {
                    Button { open(up) } label: { Label("..", systemImage: "arrow.up.left") }.accessibilityIdentifier("chat-folder-up")
                }
                if shown.isEmpty { Text(folders.isEmpty ? "这里没有子文件夹" : "没有匹配的文件夹").foregroundStyle(.secondary).font(.footnote) }
                ForEach(shown, id: \.self) { name in
                    Button { open(phoneChildFolder(dir, name)) } label: {
                        HStack {
                            Image(systemName: "folder.fill").foregroundStyle(.tint)
                            Text(name).lineLimit(1)
                            Spacer()
                            Image(systemName: "chevron.right").font(.caption).foregroundStyle(.tertiary)
                        }
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("chat-folder-child-\(name)")
                }
            } footer: {
                if more { Text("这里文件夹太多，只列出了前一部分；输入名字缩小范围。") }
            }
        }
    }

    private func open(_ path: String) {
        let target = phoneTrimFolder(path)
        query = ""; failure = nil; loading = true; browsing = target
        Task {
            do {
                let listed = try await chat.browse(target)
                guard browsing == target else { return }
                browsing = listed.path; folders = listed.folders; more = listed.more
            } catch {
                guard browsing == target else { return }
                folders = []; failure = "列不出这个文件夹：\(error.localizedDescription)"
            }
            loading = false
        }
    }

    private func choose(_ path: String) {
        selection = phoneTildeFolder(phoneTrimFolder(path), home: chat.home)
        dismiss()
    }
}

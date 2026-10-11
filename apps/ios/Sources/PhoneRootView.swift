import SwiftUI

public struct PhoneRootView: View {
    @Bindable var store: PhoneStore
    @Environment(\.colorScheme) private var colorScheme
    public init(store: PhoneStore) { self.store = store }

    public var body: some View {
        NavigationStack {
            Group {
                if let host = store.selected { session(host) }
                else { computers }
            }
            .background(canvas)
            .navigationTitle(store.selected?.name ?? "电脑")
            .toolbar {
                if store.selected != nil {
                    ToolbarItem(placement: .cancellationAction) { Button("电脑", systemImage: "chevron.left") { Task { await store.leaveSession() } } }
                    ToolbarItem(placement: .primaryAction) { Button("活动", systemImage: "list.bullet.rectangle") { store.showActivity = true } }
                } else {
                    ToolbarItem(placement: .primaryAction) { Button("配对电脑", systemImage: "plus") { store.pairSheet = true } }
                }
            }
            .sheet(isPresented: $store.pairSheet) { pairing }
            .sheet(isPresented: $store.showActivity) { activity }
            .modifier(PhoneErrorAlert(store: store, enabled: !store.pairSheet && !store.showActivity))
        }
        .tint(.blue)
    }

    // Keep the background static: system materials supply depth without decorative motion.
    private var canvas: some View {
        LinearGradient(colors: [PhonePalette.canvas, Color.blue.opacity(colorScheme == .dark ? 0.08 : 0.04), PhonePalette.canvas], startPoint: .topLeading, endPoint: .bottomTrailing)
            .ignoresSafeArea()
    }

    private var computers: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                VStack(alignment: .leading, spacing: 7) {
                    Text("ai-remote").font(.subheadline.weight(.semibold)).foregroundStyle(.blue)
                    Text("你的电脑，近在手边").font(.title2.weight(.bold))
                    Text("从这里连接电脑，查看画面或接管操作。")
                        .font(.subheadline).foregroundStyle(.secondary)
                }
                .padding(.top, 4)

                if store.hosts.isEmpty {
                    VStack(spacing: 18) {
                        Image(systemName: "desktopcomputer")
                            .font(.system(size: 40, weight: .light)).foregroundStyle(.blue)
                            .frame(width: 92, height: 92)
                            .background(.blue.opacity(0.08), in: RoundedRectangle(cornerRadius: 28))
                            .accessibilityHidden(true)
                        VStack(spacing: 8) {
                            Text("连接你的第一台电脑").font(.headline)
                            Text("在电脑上启动 ai-remote 被控端，生成配对票据，再用这台 iPhone 配对。")
                                .font(.subheadline).foregroundStyle(.secondary).multilineTextAlignment(.center)
                        }
                        Button("配对一台电脑", systemImage: "plus") { store.pairSheet = true }
                            .buttonStyle(.borderedProminent).buttonBorderShape(.capsule).controlSize(.large)
                    }
                    .frame(maxWidth: .infinity).padding(28).modifier(PhoneSurface())
                } else {
                    VStack(alignment: .leading, spacing: 12) {
                        HStack {
                            Text("已配对").font(.headline)
                            Spacer()
                            Text("\(store.hosts.count) 台电脑").font(.footnote).foregroundStyle(.secondary)
                        }
                        Text("配对保留连接身份；电脑是否可用会在连接时确认。")
                            .font(.footnote).foregroundStyle(.secondary)
                        ForEach(store.hosts) { host in computerCard(host) }
                    }
                }

                VStack(alignment: .leading, spacing: 9) {
                    Label("连接与控制，由你决定", systemImage: "lock.shield").font(.subheadline.weight(.semibold))
                    Text("配对身份保存在这台 iPhone。画面和操作通过加密连接传输，你可以随时接管或暂停智能体。退出控制页面会尝试交还控制。")
                        .font(.footnote).foregroundStyle(.secondary)
                }
                .padding(20).frame(maxWidth: .infinity, alignment: .leading).modifier(PhoneSurface())

                if store.loading {
                    ProgressView("正在准备连接…").frame(maxWidth: .infinity).padding(.vertical, 8)
                }
            }
            .padding(20).frame(maxWidth: 680).frame(maxWidth: .infinity)
        }
        .refreshable { do { try await store.refresh() } catch { store.error = error.localizedDescription } }
    }

    private func computerCard(_ host: PhoneHost) -> some View {
        Button { Task { await store.connect(host) } } label: {
            HStack(spacing: 16) {
                Image(systemName: "desktopcomputer")
                    .font(.title2.weight(.medium)).foregroundStyle(.blue)
                    .frame(width: 54, height: 54)
                    .background(.blue.opacity(0.08), in: RoundedRectangle(cornerRadius: 17))
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 7) {
                    Text(host.name).font(.headline).foregroundStyle(.primary)
                    Label(levelName(host.level), systemImage: host.level == "observe" ? "eye" : "hand.point.up.left")
                        .font(.footnote).foregroundStyle(.secondary)
                }
                Spacer(minLength: 4)
                Image(systemName: "chevron.right").font(.footnote.weight(.semibold)).foregroundStyle(.secondary)
                    .accessibilityHidden(true)
            }
            .padding(18).frame(maxWidth: .infinity, alignment: .leading)
            .modifier(PhoneSurface()).contentShape(RoundedRectangle(cornerRadius: 24))
        }
        .buttonStyle(.plain).accessibilityHint("连接这台电脑")
    }

    private func session(_ host: PhoneHost) -> some View {
        VStack(spacing: 12) {
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 12) { connectionStatus; Spacer(minLength: 8); turnButton }
                VStack(alignment: .leading, spacing: 12) { connectionStatus; turnButton }
            }
            .padding(16).modifier(PhoneGlassSurface()).padding(.horizontal, 16).padding(.top, 8)
            screenView
        }
        .safeAreaInset(edge: .bottom, spacing: 0) { sessionControls.padding(16) }
    }

    private var connectionStatus: some View {
        HStack(spacing: 10) {
            Image(systemName: !store.connected ? (store.image == nil && store.error == nil ? "antenna.radiowaves.left.and.right" : "wifi.exclamationmark") : store.ownsControls ? "hand.point.up.left.fill" : "eye.fill")
                .foregroundStyle(!store.connected ? Color.secondary : store.ownsControls ? Color.orange : Color.blue)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                Text(sessionTitle).font(.subheadline.weight(.semibold))
                Text(sessionDetail).font(.caption).foregroundStyle(.secondary)
            }
        }
        .accessibilityElement(children: .combine)
    }

    private var sessionTitle: String {
        guard store.connected else {
            return store.image != nil ? "画面已中断" : store.error == nil ? "等待实时画面" : "画面未就绪"
        }
        return store.ownsControls ? "你正在操作" : store.driver == "paused" ? "智能体已暂停" : "正在观看"
    }

    private var sessionDetail: String {
        guard store.connected else { return store.image != nil ? "画面已停止更新" : "等待实时画面" }
        return store.ownsControls ? "轻点、拖动或输入文字" : levelName(store.level)
    }

    private var turnButton: some View {
        Button(store.ownsControls ? "交还" : "接管", systemImage: store.ownsControls ? "arrow.uturn.backward" : "hand.point.up.left") {
            Task { await store.setTurn(store.ownsControls ? "hand_back" : "take_over") }
        }
        .buttonStyle(.borderedProminent).buttonBorderShape(.capsule).controlSize(.regular)
        .disabled(!store.ownsControls && (!store.mayControl || !store.connected))
    }

    private var sessionControls: some View {
        VStack(spacing: 12) {
            if store.ownsControls {
                HStack(spacing: 10) {
                    TextField("输入到电脑", text: $store.text)
                        .textFieldStyle(.plain).padding(.horizontal, 15).frame(minHeight: 44)
                        .modifier(PhoneSurface(cornerRadius: 18))
                        .disabled(!store.connected).onSubmit { if store.connected { store.sendText() } }
                    Button("发送", systemImage: "arrow.up.circle.fill") { store.sendText() }
                        .labelStyle(.iconOnly).font(.title).frame(minWidth: 44, minHeight: 44)
                        .disabled(store.text.isEmpty || !store.connected)
                }
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        ForEach(["Return", "Tab", "Escape", "Backspace"], id: \.self) { key in
                            Button(key) { store.input(tool: "keyboard", args: ["keys": .string(key)]) }
                                .font(.caption.weight(.medium)).frame(minHeight: 44).padding(.horizontal, 12)
                                .modifier(PhoneSurface(cornerRadius: 16))
                        }
                        Button("向上滚动", systemImage: "arrow.up") { scroll("up") }.labelStyle(.iconOnly).frame(width: 44, height: 44).modifier(PhoneSurface(cornerRadius: 16))
                        Button("向下滚动", systemImage: "arrow.down") { scroll("down") }.labelStyle(.iconOnly).frame(width: 44, height: 44).modifier(PhoneSurface(cornerRadius: 16))
                    }
                    .buttonStyle(.plain).disabled(!store.connected)
                }
            } else if store.connected {
                Text(store.mayControl ? "接管后，可轻点、拖动和输入文字" : "当前权限仅可观看电脑画面")
                    .font(.footnote).foregroundStyle(.secondary).multilineTextAlignment(.center)
            }

            ViewThatFits(in: .horizontal) {
                HStack(spacing: 12) { agentButton; Spacer(minLength: 4); permissionLabel }
                VStack(spacing: 8) { agentButton; permissionLabel }
            }
            .padding(.horizontal, 14).padding(.vertical, 8).modifier(PhoneGlassSurface())
        }
    }

    private var agentButton: some View {
        Button(store.driver == "paused" ? "继续智能体" : "暂停智能体", systemImage: store.driver == "paused" ? "play.fill" : "pause.fill") {
            Task { await store.setTurn(store.driver == "paused" ? "resume" : "pause") }
        }
        .font(.subheadline.weight(.medium)).frame(minHeight: 44)
        // Pause remains an emergency action even before video arrives or after it fails.
        .disabled(store.driver == "paused" && !store.mayControl)
    }

    private var permissionLabel: some View {
        Label(levelName(store.level), systemImage: "lock.shield").font(.caption).foregroundStyle(.secondary)
    }

    private var screenView: some View {
        GeometryReader { area in
            if let picture = store.image {
                let ratio = CGFloat(picture.width) / CGFloat(picture.height)
                let width = min(area.size.width, area.size.height * ratio), height = width / ratio
                Image(decorative: picture, scale: 1).resizable().frame(width: width, height: height)
                    .contentShape(Rectangle())
                    .gesture(DragGesture(minimumDistance: 0).onEnded { gesture in
                        guard store.connected, let screen = store.screen,
                              let start = screen.point(u: gesture.startLocation.x / width, v: gesture.startLocation.y / height),
                              let end = screen.point(u: gesture.location.x / width, v: gesture.location.y / height) else { return }
                        let drag = hypot(gesture.translation.width, gesture.translation.height) > 10
                        var args: [String: JSONValue] = ["action": .string(drag ? "drag" : "click"), "x": .number(start.0), "y": .number(start.1), "space": .string("screen")]
                        if drag { args["to_x"] = .number(end.0); args["to_y"] = .number(end.1) }
                        store.input(tool: "mouse", args: args)
                    }).position(x: area.size.width / 2, y: area.size.height / 2)
            } else {
                VStack(spacing: 14) {
                    if store.error == nil { ProgressView().controlSize(.large) }
                    else { Image(systemName: "wifi.exclamationmark").font(.largeTitle).foregroundStyle(.secondary) }
                    Text(store.error == nil ? "正在连接 \(store.selected?.name ?? "电脑")" : "暂时无法显示画面")
                        .font(.headline).multilineTextAlignment(.center)
                    Text(store.error == nil ? "第一帧到达后显示真实画面" : "确认被控端正在运行，再返回电脑列表重新连接。")
                        .font(.footnote).foregroundStyle(.secondary).multilineTextAlignment(.center)
                }
                .padding(24).frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        // Controls sit outside this surface so no blur or overlay hides the remote desktop.
        .background(PhonePalette.screen).frame(maxHeight: .infinity)
    }

    private func scroll(_ direction: String) {
        guard store.connected, let screen = store.screen else { return }
        store.input(tool: "mouse", args: ["action": .string("scroll"), "direction": .string(direction), "amount": .number(3), "x": .number(screen.x + screen.width/2), "y": .number(screen.y + screen.height/2), "space": .string("screen")])
    }

    private var pairing: some View {
        NavigationStack {
            Form {
                Section("在电脑上生成票据") {
                    Label("配对属于你信任的电脑", systemImage: "lock.shield").font(.headline)
                    Text("启动 ai-remote host，再运行 ai-remote pair-code --level control。").font(.callout)
                    Text("可以通过通用剪贴板把票据复制到 iPhone。").font(.footnote).foregroundStyle(.secondary)
                }
                Section("一次性配对票据") {
                    TextEditor(text: $store.pairingCode).frame(minHeight: 140).font(.system(.footnote, design: .monospaced)).autocorrectionDisabled()
                        .ticketCapitalizationDisabled().accessibilityIdentifier("pairing-ticket")
                        .scrollContentBackground(.hidden).padding(8).modifier(PhoneSurface(cornerRadius: 18))
                    Button { Task { await store.pair() } } label: {
                        HStack {
                            Spacer()
                            if store.loading { ProgressView() }
                            Text(store.loading ? "正在配对…" : "配对").fontWeight(.semibold)
                            Spacer()
                        }.frame(minHeight: 32)
                    }
                    .buttonStyle(.borderedProminent).buttonBorderShape(.capsule).controlSize(.large)
                    .disabled(store.loading || store.pairingCode.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    .accessibilityIdentifier("pair-submit")
                }
                if let code = store.safetyCode {
                    Section("核对安全码") {
                        Label("请在两台设备上确认", systemImage: "checkmark.shield")
                        Text(code).font(.system(.title3, design: .monospaced)).textSelection(.enabled)
                        Text("请确认电脑上显示的是相同的三个单词。配对票据使用一次后失效。").font(.footnote).foregroundStyle(.secondary)
                    }
                }
            }
            .navigationTitle("配对电脑")
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("完成") { store.pairSheet = false } } }
        }.modifier(PhoneErrorAlert(store: store))
    }

    private var activity: some View {
        NavigationStack {
            List {
                if !store.pending.isEmpty {
                    Section("待批准") {
                        ForEach(store.pending, id: \.text) { request in
                            VStack(alignment: .leading, spacing: 10) {
                                Label(request["tool"].string ?? "请求", systemImage: "hand.raised").font(.headline)
                                Text(request["intent"].string ?? request["args_preview"].string ?? "")
                                Button("拒绝", role: .destructive) { Task { await store.deny(request["id"].string ?? "") } }
                                    .buttonStyle(.bordered).buttonBorderShape(.capsule)
                            }.padding(.vertical, 6)
                        }
                        Text("当前 iPhone 版本尚未实现 Face ID 审批签名。允许操作请在被控电脑确认。").font(.footnote).foregroundStyle(.secondary)
                    }
                }
                Section("最近活动") {
                    if store.activity.isEmpty {
                        Label("还没有活动记录", systemImage: "clock").foregroundStyle(.secondary)
                        Text("本次连接产生的操作、审批与结果会显示在这里。").font(.footnote).foregroundStyle(.secondary)
                    }
                    ForEach(Array(store.activity.reversed().enumerated()), id: \.offset) { _, event in
                        VStack(alignment: .leading, spacing: 6) {
                            Text(event["tool"].string ?? event["kind"].string ?? "活动").font(.headline)
                            Text(event["intent"].string ?? event["outcome"].string ?? event["decision"].string ?? event["task"].string ?? "")
                                .font(.footnote).foregroundStyle(.secondary)
                        }.padding(.vertical, 4)
                    }
                }
            }
            .navigationTitle("活动与审批")
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("完成") { store.showActivity = false } } }
            .task { await store.refreshPending() }
        }.modifier(PhoneErrorAlert(store: store))
    }

    private func levelName(_ level: String?) -> String { switch level { case "admin": "管理权限"; case "control": "控制权限"; case "observe": "仅观看"; default: "等待权限信息" } }
}

private enum PhonePalette {
    #if os(iOS)
    static let canvas = Color(uiColor: .systemGroupedBackground)
    static let surface = Color(uiColor: .secondarySystemGroupedBackground)
    static let screen = Color(uiColor: .secondarySystemBackground)
    #else
    static let canvas = Color(nsColor: .windowBackgroundColor)
    static let surface = Color(nsColor: .controlBackgroundColor)
    static let screen = Color(nsColor: .underPageBackgroundColor)
    #endif
}

/// Content cards use standard materials; Liquid Glass is reserved for the controls.
private struct PhoneSurface: ViewModifier {
    var cornerRadius: CGFloat = 24
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
    @Environment(\.colorSchemeContrast) private var contrast
    func body(content: Content) -> some View {
        let shape = RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
        content
            .background {
                if reduceTransparency || contrast == .increased { shape.fill(PhonePalette.surface) }
                else { shape.fill(.regularMaterial) }
            }
            .overlay { shape.strokeBorder(.primary.opacity(contrast == .increased ? 0.3 : 0.08), lineWidth: 1) }
    }
}

private struct PhoneGlassSurface: ViewModifier {
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
    @Environment(\.colorSchemeContrast) private var contrast
    @ViewBuilder func body(content: Content) -> some View {
        // Older Swift/Xcode releases keep the iOS 17 / macOS 14 material implementation.
        #if compiler(>=6.2)
        if #available(iOS 26.0, macOS 26.0, *), !reduceTransparency, contrast != .increased {
            content.glassEffect(.regular, in: RoundedRectangle(cornerRadius: 24, style: .continuous))
        } else {
            content.modifier(PhoneSurface())
        }
        #else
        content.modifier(PhoneSurface())
        #endif
    }
}

// Present errors from the visible sheet, so failed pairing/approval calls stay actionable.
private struct PhoneErrorAlert: ViewModifier {
    var store: PhoneStore
    var enabled = true
    func body(content: Content) -> some View {
        content.alert("需要关注", isPresented: Binding(get: { enabled && store.error != nil }, set: { if !$0 { store.error = nil } })) {
            Button("好", role: .cancel) { store.error = nil }
        } message: { Text(store.error ?? "") }
    }
}

private extension View {
    @ViewBuilder func ticketCapitalizationDisabled() -> some View {
        #if os(iOS)
        self.textInputAutocapitalization(.never)
        #else
        self
        #endif
    }
}

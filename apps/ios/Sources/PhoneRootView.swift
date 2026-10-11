import SwiftUI

public struct PhoneRootView: View {
    @Bindable var store: PhoneStore
    @Environment(\.colorScheme) private var colorScheme
    @PhoneViewState<Bool> private var fullscreen = false
    @PhoneViewState<Bool> private var controlsExpanded = true
    @PhoneViewState<Bool> private var showKeyboard = false
    @PhoneViewState<PhoneViewportRequest> private var viewportRequest = PhoneViewportRequest()
    @PhoneViewState<EdgeInsets> private var viewportInsets = EdgeInsets()
    @FocusState private var textFocused: Bool
    #if DEBUG
    private var fixtureInputObserver: ((String, [String: JSONValue]) -> Void)? = nil
    @PhoneViewState<Int> private var fullscreenToggleCount = 0
    @PhoneViewState<String> private var fullscreenChangeReason = "initial"
    public init(store: PhoneStore, fixtureInputObserver: @escaping (String, [String: JSONValue]) -> Void) {
        self.store = store
        self.fixtureInputObserver = fixtureInputObserver
    }
    #endif
    public init(store: PhoneStore) { self.store = store }

    public var body: some View {
        Group {
            if fullscreen, let host = store.selected {
                // A full-window session has no navigation container consuming its
                // layout proposal. It keeps the same store and stream subscription.
                session(host)
            } else {
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
                    .modifier(PhoneSessionChrome(fullscreen: false))
                }
            }
        }
        .sheet(isPresented: $store.pairSheet) { pairing }
        .sheet(isPresented: $store.showActivity) { activity }
        .modifier(PhoneErrorAlert(store: store, enabled: !store.pairSheet && !store.showActivity))
        .onChange(of: store.selected?.name) { _, _ in
            fullscreen = false; showKeyboard = false; textFocused = false
            #if DEBUG
            fullscreenChangeReason = "host changed"
            #endif
            controlsExpanded = true
            requestViewport(.fit)
        }
        .modifier(PhoneSessionChrome(fullscreen: fullscreen && store.selected != nil))
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
        ZStack {
            screenView
            VStack(spacing: 0) {
                HStack(spacing: 8) {
                    HStack(spacing: 6) {
                        Circle().fill(store.connected ? Color.green : Color.orange).frame(width: 6, height: 6)
                        Text(sessionTitle).font(.caption.weight(.medium)).lineLimit(1)
                    }
                    .padding(.horizontal, 12).frame(minHeight: 36)
                    .modifier(PhoneGlassSurface())
                    .accessibilityLabel("\(host.name)，\(sessionTitle)，\(sessionDetail)")
                    Spacer(minLength: 8)
                    Button { requestViewport(.fit) } label: {
                        Image(systemName: "arrow.down.right.and.arrow.up.left").frame(width: 44, height: 44).contentShape(Rectangle())
                    }
                        .buttonStyle(.plain).modifier(PhoneGlassSurface()).accessibilityLabel("适合画面")
                        .accessibilityIdentifier("session-fit")
                    Button {
                        fullscreen.toggle()
                        #if DEBUG
                        fullscreenToggleCount += 1
                        fullscreenChangeReason = "toggle"
                        #endif
                    } label: {
                        Image(systemName: fullscreen ? "arrow.down.forward.and.arrow.up.backward" : "arrow.up.backward.and.arrow.down.forward")
                            .frame(width: 44, height: 44).contentShape(Rectangle())
                    }
                    .buttonStyle(.plain).modifier(PhoneGlassSurface())
                    .accessibilityLabel(fullscreen ? "退出全屏" : "全屏")
                    .accessibilityValue(fullscreenDiagnostic)
                    .accessibilityIdentifier("session-fullscreen")
                }
                Spacer(minLength: 8)
                floatingControls
                    .frame(maxWidth: .infinity, alignment: .trailing)
            }
            .padding(.leading, viewportInsets.leading + 12)
            .padding(.trailing, viewportInsets.trailing + 12)
            .padding(.top, viewportInsets.top + 8)
            .padding(.bottom, viewportInsets.bottom + 8)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .ignoresSafeArea(.container, edges: fullscreen ? .all : [])
        .background(.black)
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if showKeyboard { keyboardDrawer }
        }
        .onChange(of: store.ownsControls) { _, owns in
            if !owns { showKeyboard = false; textFocused = false }
        }
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
        .frame(minHeight: 44).accessibilityIdentifier("session-take-over")
        .disabled(!store.ownsControls && (!store.mayControl || !store.connected))
    }

    private var floatingControls: some View {
        HStack(spacing: 6) {
            turnButton
            agentButton.labelStyle(.iconOnly).frame(width: 44, height: 44)
            if controlsExpanded {
                Button("键盘", systemImage: "keyboard") {
                    showKeyboard.toggle()
                    textFocused = showKeyboard
                }
                .labelStyle(.iconOnly).frame(width: 44, height: 44)
                .disabled(!store.connected || !store.ownsControls)
                .accessibilityIdentifier("session-keyboard")
                Menu {
                    Button("填满屏幕", systemImage: "arrow.up.left.and.arrow.down.right") { requestViewport(.fill) }
                    Button("适合画面", systemImage: "arrow.down.right.and.arrow.up.left") { requestViewport(.fit) }
                    Menu("快捷键") {
                        ForEach(["Return", "Tab", "Escape", "Backspace"], id: \.self) { key in
                            Button(key) { sendKey(key) }
                        }
                    }.disabled(!store.connected || !store.ownsControls)
                    Button("向上滚动", systemImage: "arrow.up") { scroll("up") }
                        .disabled(!store.connected || !store.ownsControls)
                    Button("向下滚动", systemImage: "arrow.down") { scroll("down") }
                        .disabled(!store.connected || !store.ownsControls)
                    Button("活动与审批", systemImage: "list.bullet.rectangle") { store.showActivity = true }
                        .accessibilityIdentifier("session-activity")
                    Button("退出连接", systemImage: "rectangle.portrait.and.arrow.right", role: .destructive) {
                        Task { await store.leaveSession() }
                    }.accessibilityIdentifier("session-exit")
                } label: { Image(systemName: "ellipsis").frame(width: 44, height: 44) }
                .accessibilityLabel("更多控制").accessibilityIdentifier("session-menu")
            }
            Button(controlsExpanded ? "收起控制栏" : "展开控制栏", systemImage: controlsExpanded ? "chevron.down" : "chevron.up") {
                controlsExpanded.toggle()
            }
            .labelStyle(.iconOnly).frame(width: 44, height: 44)
            .accessibilityIdentifier("session-controls-toggle")
        }
        .buttonStyle(.plain).padding(6).modifier(PhoneGlassSurface())
    }

    private var keyboardDrawer: some View {
        HStack(spacing: 8) {
            TextField("输入到电脑", text: $store.text)
                .textFieldStyle(.plain).padding(.horizontal, 12).frame(minHeight: 44)
                .modifier(PhoneSurface(cornerRadius: 16)).focused($textFocused)
                .disabled(!store.connected || !store.ownsControls)
                .onSubmit { sendText() }.accessibilityIdentifier("session-text")
            Button("发送", systemImage: "arrow.up.circle.fill") { sendText() }
                .labelStyle(.iconOnly).font(.title2).frame(width: 44, height: 44)
                .disabled(store.text.isEmpty || !store.connected || !store.ownsControls)
                .accessibilityIdentifier("session-send")
            Button("收起键盘", systemImage: "keyboard.chevron.compact.down") {
                textFocused = false; showKeyboard = false
            }
            .labelStyle(.iconOnly).frame(width: 44, height: 44)
            .accessibilityIdentifier("session-keyboard-close")
        }
        .padding(.horizontal, 12).padding(.vertical, 8).background(.bar)
    }

    private func requestViewport(_ mode: PhoneViewportRequest.Mode) {
        viewportRequest = PhoneViewportRequest(mode: mode, revision: viewportRequest.revision + 1)
    }

    private var fullscreenDiagnostic: String {
        #if DEBUG
        if fixtureInputObserver != nil {
            return JSONValue.object(["fullscreen": .bool(fullscreen), "toggle_count": .number(Double(fullscreenToggleCount)),
                                     "reason": .string(fullscreenChangeReason)]).text
        }
        #endif
        return fullscreen ? "全屏中" : "标准显示"
    }

    private func sendKey(_ key: String) {
        guard store.connected, store.ownsControls else { return }
        store.input(tool: "keyboard", args: ["keys": .string(key)])
    }

    private func sendText() {
        guard store.connected, store.ownsControls else { return }
        store.sendText()
    }

    private var agentButton: some View {
        Button(store.driver == "paused" ? "继续智能体" : "暂停智能体", systemImage: store.driver == "paused" ? "play.fill" : "pause.fill") {
            Task { await store.setTurn(store.driver == "paused" ? "resume" : "pause") }
        }
        .font(.subheadline.weight(.medium)).frame(minHeight: 44)
        .accessibilityIdentifier("session-agent")
        // Pause remains an emergency action even before video arrives or after it fails.
        .disabled(store.driver == "paused" && !store.mayControl)
    }

    private var permissionLabel: some View {
        Label(levelName(store.level), systemImage: "lock.shield").font(.caption).foregroundStyle(.secondary)
    }

    private var screenView: some View {
        GeometryReader { area in
            if let picture = store.image {
                #if os(iOS)
                nativeViewport(picture)
                #else
                let ratio = CGFloat(picture.width) / CGFloat(picture.height)
                let width = min(area.size.width, area.size.height * ratio), height = width / ratio
                Image(decorative: picture, scale: 1).resizable().frame(width: width, height: height)
                    .contentShape(Rectangle())
                    .gesture(DragGesture(minimumDistance: 0).onEnded { gesture in
                        guard let start = PhoneViewportGeometry.normalizedPoint(gesture.startLocation, imageSize: CGSize(width: width, height: height)),
                              let end = PhoneViewportGeometry.normalizedPoint(gesture.location, imageSize: CGSize(width: width, height: height)) else { return }
                        let drag = hypot(gesture.translation.width, gesture.translation.height) > 10
                        sendPointer(PhonePointerGesture(start: start, end: end, isDrag: drag))
                    }).position(x: area.size.width / 2, y: area.size.height / 2)
                #endif
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
        .background(.black).frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    #if os(iOS)
    private func nativeViewport(_ picture: CGImage) -> some View {
        var viewport = PhoneRemoteViewport(image: picture, allowsInput: store.connected && store.ownsControls,
                                           request: viewportRequest, onGesture: sendPointer, onInsets: { insets in
            let next = EdgeInsets(top: insets.top, leading: insets.left, bottom: insets.bottom, trailing: insets.right)
            if viewportInsets != next { viewportInsets = next }
        })
        #if DEBUG
        viewport.diagnosticsEnabled = fixtureInputObserver != nil
        #endif
        return viewport
    }
    #endif

    private func sendPointer(_ gesture: PhonePointerGesture) {
        guard store.connected, store.ownsControls, let screen = store.screen,
              let start = screen.point(u: Double(gesture.start.x), v: Double(gesture.start.y)),
              let end = screen.point(u: Double(gesture.end.x), v: Double(gesture.end.y)) else { return }
        var args: [String: JSONValue] = ["action": .string(gesture.isDrag ? "drag" : "click"),
                                       "x": .number(start.0), "y": .number(start.1), "space": .string("screen")]
        if gesture.isDrag { args["to_x"] = .number(end.0); args["to_y"] = .number(end.1) }
        #if DEBUG
        if let observer = fixtureInputObserver { observer("mouse", args); return }
        #endif
        store.input(tool: "mouse", args: args)
    }

    private func scroll(_ direction: String) {
        guard store.connected, store.ownsControls, let screen = store.screen else { return }
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
                    TextEditor(text: $store.pairingCode).frame(height: 140).font(.system(.footnote, design: .monospaced)).autocorrectionDisabled()
                        .ticketCapitalizationDisabled().accessibilityIdentifier("pairing-ticket")
                        .scrollContentBackground(.hidden).padding(8).modifier(PhoneSurface(cornerRadius: 18))
                }
                if let code = store.safetyCode {
                    Section("核对安全码") {
                        Label("请在两台设备上确认", systemImage: "checkmark.shield")
                        Text(code).font(.system(.title3, design: .monospaced)).textSelection(.enabled)
                        Text("请确认电脑上显示的是相同的三个单词。配对票据使用一次后失效。").font(.footnote).foregroundStyle(.secondary)
                    }
                }
            }
            .safeAreaInset(edge: .bottom, spacing: 0) {
                // Keep the primary action above the keyboard while the ticket scrolls.
                Button { Task { await store.pair() } } label: {
                    HStack(spacing: 8) {
                        if store.loading { ProgressView() }
                        Text(store.loading ? "正在配对…" : "配对").fontWeight(.semibold)
                    }.frame(maxWidth: .infinity, minHeight: 32)
                }
                .buttonStyle(.borderedProminent).buttonBorderShape(.capsule).controlSize(.large)
                .disabled(store.loading || store.pairingCode.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                .accessibilityIdentifier("pair-submit")
                .padding(.horizontal, 20).padding(.vertical, 12)
                .background(.bar)
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

private struct PhoneSessionChrome: ViewModifier {
    let fullscreen: Bool
    func body(content: Content) -> some View {
        #if os(iOS)
        content.navigationBarTitleDisplayMode(.inline)
            .toolbar(fullscreen ? .hidden : .visible, for: .navigationBar)
            .statusBarHidden(fullscreen)
        #else
        content
        #endif
    }
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

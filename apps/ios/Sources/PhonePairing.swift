import SwiftUI
#if os(iOS)
import AVFoundation
import UIKit
#endif

/// Pairing a computer: scan the QR code it shows, or type its eight-digit pair code. The full
/// ticket stays behind a link for computers the phone cannot reach over the network.
struct PhonePairSheet: View {
    @Bindable var store: PhoneStore
    @State private var scanning = false
    @State private var showNetwork = false
    #if os(iOS)
    @State private var codeFocused = false
    #else
    @FocusState private var codeFocused: Bool
    #endif

    private var ready: Bool {
        store.useTicket
            ? !store.pairingTicket.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            : PairCodeText.digits(store.pairingCode) != nil
    }

    var body: some View {
        NavigationStack {
            Form {
                if let code = store.safetyCode {
                    Section {
                        VStack(alignment: .leading, spacing: 10) {
                            Label(store.pairedHost.map { "已与 \($0) 配对" } ?? "已配对", systemImage: "checkmark.circle.fill")
                                .font(.headline).foregroundStyle(.green)
                            Text(code.split(whereSeparator: { $0 == "-" || $0 == " " }).joined(separator: " "))
                                .font(.system(.largeTitle, design: .rounded).weight(.semibold))
                                .minimumScaleFactor(0.5).lineLimit(1).textSelection(.enabled)
                                .accessibilityIdentifier("pair-safety-words")
                            Text("电脑上应显示同样的三个词。不一致说明有别人用了这个配对码：在电脑上移除这台设备，并在这里解除配对。")
                                .font(.footnote).foregroundStyle(.secondary)
                        }.padding(.vertical, 6)
                    } header: { Text("核对安全码") }
                }
                if store.useTicket {
                    Section {
                        TextEditor(text: $store.pairingTicket).frame(height: 140).font(.system(.footnote, design: .monospaced)).autocorrectionDisabled()
                            .ticketCapitalizationDisabled().accessibilityIdentifier("pairing-ticket")
                            .scrollContentBackground(.hidden).padding(8).modifier(PhoneSurface(cornerRadius: 18))
                        Button("改用配对码") { store.useTicket = false }.accessibilityIdentifier("pair-use-code")
                    } header: { Text("完整票据") } footer: {
                        Text("电脑和设备之间不通网络时使用：在电脑的配对窗口里展开“使用完整票据”，复制后粘贴到这里。")
                    }
                } else {
                    if store.network?.pairCode == false {
                        Section {
                            Label("仅限局域网且没打开“在局域网里自动发现电脑”时，配对码找不到电脑。", systemImage: "wifi.exclamationmark")
                                .font(.subheadline)
                            Button("改用完整票据") { store.useTicket = true }
                            Button("打开网络设置") { showNetwork = true }
                        }.accessibilityIdentifier("pair-code-needs-discovery")
                    }
                    #if os(iOS)
                    Section {
                        Button { scanning = true } label: {
                            HStack(spacing: 14) {
                                Image(systemName: "qrcode.viewfinder").font(.system(size: 30, weight: .regular)).frame(width: 40)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text("扫描二维码").font(.headline)
                                    Text("对准电脑上配对窗口里的二维码").font(.footnote).foregroundStyle(.secondary)
                                }
                            }.padding(.vertical, 6)
                        }.accessibilityIdentifier("pair-scan")
                    }
                    #endif
                    Section {
                        #if os(iOS)
                        // Grouped inside the keystroke itself: rewriting the text after SwiftUI publishes the
                        // edit races the next key, and fast typing lost a digit.
                        PairCodeField(text: $store.pairingCode, focused: $codeFocused) { ticket in
                            store.pairingTicket = ticket; store.useTicket = true
                        }
                        .frame(height: 56).padding(.vertical, 6)
                        #else
                        TextField("0000 0000", text: $store.pairingCode)
                            .onChange(of: store.pairingCode) { old, value in
                                if PairCodeText.isTicket(value) {
                                    store.pairingTicket = value; store.pairingCode = old; store.useTicket = true
                                } else if PairCodeText.grouped(value) != value {
                                    store.pairingCode = PairCodeText.grouped(value)
                                }
                            }
                            .font(.system(size: 40, weight: .semibold, design: .rounded)).monospacedDigit()
                            .tracking(3).multilineTextAlignment(.center)
                            .numberPadKeyboard().autocorrectionDisabled().focused($codeFocused)
                            .accessibilityIdentifier("pairing-code")
                            .padding(.vertical, 6)
                        #endif
                        Button("改用完整票据") { store.useTicket = true }.font(.footnote).accessibilityIdentifier("pair-use-ticket")
                    } header: { Text("或输入配对码") } footer: {
                        Text("在电脑上打开 ai-remote → 本设备 → 显示配对码。配对码只能用一次，10 分钟内有效。")
                    }
                }
            }
            .safeAreaInset(edge: .bottom, spacing: 0) {
                // Keep the primary action above the keyboard.
                Button { codeFocused = false; Task { await store.pair() } } label: {
                    HStack(spacing: 8) {
                        if store.loading { ProgressView() }
                        Text(store.loading ? (store.useTicket ? "正在配对…" : "正在查找这台电脑…") : "配对").fontWeight(.semibold)
                    }.frame(maxWidth: .infinity, minHeight: 32)
                }
                .buttonStyle(.borderedProminent).buttonBorderShape(.capsule).controlSize(.large)
                .disabled(store.loading || !ready)
                .accessibilityIdentifier("pair-submit")
                .padding(.horizontal, 20).padding(.vertical, 12)
                .background(.bar)
            }
            .navigationTitle("配对电脑")
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("完成") { store.pairSheet = false } } }
            #if os(iOS)
            .fullScreenCover(isPresented: $scanning) {
                PhoneScanView { digits in
                    scanning = false
                    store.useTicket = false
                    store.pairingCode = PairCodeText.grouped(digits)
                    Task { await store.pair() }
                } cancel: { scanning = false }
            }
            #endif
            .sheet(isPresented: $showNetwork) { PhoneNetworkSheet(store: store) }
        }.modifier(PhoneErrorAlert(store: store, enabled: !showNetwork))
    }
}

private extension View {
    @ViewBuilder func numberPadKeyboard() -> some View {
        #if os(iOS)
        self.keyboardType(.numberPad)
        #else
        self
        #endif
    }
}

#if os(iOS)
/// The camera, looking for a pair code's QR code. Asks for camera access the first time; a denied
/// permission gets a way to Settings instead of a black screen.
struct PhoneScanView: View {
    var found: (String) -> Void
    var cancel: () -> Void
    @State private var access = AVCaptureDevice.authorizationStatus(for: .video)
    @State private var notACode = false

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            switch access {
            case .authorized:
                PhoneQRCamera { text in
                    if let digits = PairCodeText.digits(text) { found(digits); return true }
                    notACode = true
                    return false
                }.ignoresSafeArea()
                RoundedRectangle(cornerRadius: 28, style: .continuous)
                    .strokeBorder(.white.opacity(0.9), lineWidth: 3)
                    .frame(width: 250, height: 250)
                    .shadow(color: .black.opacity(0.4), radius: 12)
                    .accessibilityHidden(true)
            case .notDetermined:
                ProgressView().tint(.white)
                    .task { access = await AVCaptureDevice.requestAccess(for: .video) ? .authorized : .denied }
            default:
                VStack(spacing: 14) {
                    Image(systemName: "camera.fill").font(.system(size: 40)).foregroundStyle(.white.opacity(0.8))
                    Text("需要相机权限才能扫码").font(.headline).foregroundStyle(.white)
                    Text("在“设置”里允许 ai-remote 使用相机，或返回输入 8 位配对码。").font(.footnote)
                        .foregroundStyle(.white.opacity(0.7)).multilineTextAlignment(.center)
                    Button("打开设置") {
                        if let url = URL(string: UIApplication.openSettingsURLString) { UIApplication.shared.open(url) }
                    }.buttonStyle(.borderedProminent)
                }.padding(32)
            }
            VStack {
                HStack {
                    Spacer()
                    Button { cancel() } label: {
                        Image(systemName: "xmark").font(.system(size: 17, weight: .semibold)).foregroundStyle(.white)
                            .frame(width: 40, height: 40).background(.ultraThinMaterial, in: Circle())
                    }.accessibilityLabel("关闭").accessibilityIdentifier("scan-close")
                }.padding(.horizontal, 20).padding(.top, 12)
                Spacer()
                Text(notACode ? "这不是 ai-remote 的配对二维码" : "对准电脑上配对窗口里的二维码")
                    .font(.callout.weight(.medium)).foregroundStyle(.white)
                    .padding(.horizontal, 18).padding(.vertical, 10).background(.ultraThinMaterial, in: Capsule())
                    .padding(.bottom, 40)
            }
        }
    }
}

/// A camera preview that reports QR codes. `found` returns whether to stop looking.
private struct PhoneQRCamera: UIViewControllerRepresentable {
    var found: (String) -> Bool
    func makeUIViewController(context: Context) -> QRCameraController {
        let controller = QRCameraController()
        controller.found = found
        return controller
    }
    func updateUIViewController(_ controller: QRCameraController, context: Context) { controller.found = found }
}

final class QRCameraController: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
    var found: ((String) -> Bool)?
    private let session = AVCaptureSession()
    private var preview: AVCaptureVideoPreviewLayer?
    private var done = false
    private var last = ""

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black
        guard let camera = AVCaptureDevice.default(for: .video), let input = try? AVCaptureDeviceInput(device: camera),
              session.canAddInput(input) else { return }
        session.addInput(input)
        let output = AVCaptureMetadataOutput()
        guard session.canAddOutput(output) else { return }
        session.addOutput(output)
        output.setMetadataObjectsDelegate(self, queue: .main)
        output.metadataObjectTypes = [.qr]
        let layer = AVCaptureVideoPreviewLayer(session: session)
        layer.videoGravity = .resizeAspectFill
        view.layer.addSublayer(layer)
        preview = layer
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        preview?.frame = view.bounds
    }

    override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        let session = self.session
        // `startRunning` blocks; Apple asks for it off the main thread.
        DispatchQueue.global(qos: .userInitiated).async { if !session.isRunning { session.startRunning() } }
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        let session = self.session
        DispatchQueue.global(qos: .userInitiated).async { if session.isRunning { session.stopRunning() } }
    }

    nonisolated func metadataOutput(_ output: AVCaptureMetadataOutput, didOutput objects: [AVMetadataObject], from connection: AVCaptureConnection) {
        let texts = objects.compactMap { ($0 as? AVMetadataMachineReadableCodeObject)?.stringValue }
        MainActor.assumeIsolated {
            guard !done, let text = texts.first, text != last else { return }
            last = text
            if found?(text) == true {
                done = true
                UINotificationFeedbackGenerator().notificationOccurred(.success)
            }
        }
    }
}
#endif

#if os(iOS)
/// The pair code field. Each keystroke is filtered and grouped (`4821 0937`) before UIKit applies it,
/// so the field never shows ungrouped text and a key typed while the previous one is still being
/// published is not lost. Pasting a ticket hands it to `onTicket` instead.
struct PairCodeField: UIViewRepresentable {
    @Binding var text: String
    @Binding var focused: Bool
    var onTicket: (String) -> Void

    func makeUIView(context: Context) -> UITextField {
        let field = UITextField()
        let base = UIFont.monospacedDigitSystemFont(ofSize: 40, weight: .semibold)
        field.font = base.fontDescriptor.withDesign(.rounded).map { UIFont(descriptor: $0, size: 40) } ?? base
        field.defaultTextAttributes[.kern] = 3
        field.textAlignment = .center
        field.keyboardType = .numberPad
        field.autocorrectionType = .no
        field.spellCheckingType = .no
        field.placeholder = "0000 0000"
        field.accessibilityIdentifier = "pairing-code"
        field.delegate = context.coordinator
        field.text = text
        field.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        return field
    }

    func updateUIView(_ field: UITextField, context: Context) {
        context.coordinator.parent = self
        if field.text != text { field.text = text }
        if focused, !field.isFirstResponder {
            DispatchQueue.main.async { field.becomeFirstResponder() }
        } else if !focused, field.isFirstResponder {
            DispatchQueue.main.async { field.resignFirstResponder() }
        }
    }

    func makeCoordinator() -> Coordinator { Coordinator(parent: self) }

    final class Coordinator: NSObject, UITextFieldDelegate {
        var parent: PairCodeField
        init(parent: PairCodeField) { self.parent = parent }

        func textField(_ field: UITextField, shouldChangeCharactersIn range: NSRange, replacementString string: String) -> Bool {
            let current = field.text ?? ""
            guard let span = Range(range, in: current) else { return false }
            let edited = current.replacingCharacters(in: span, with: string)
            if PairCodeText.isTicket(edited) {
                parent.onTicket(edited)
                return false
            }
            let grouped = PairCodeText.grouped(edited)
            field.text = grouped
            let end = field.endOfDocument
            field.selectedTextRange = field.textRange(from: end, to: end)
            parent.text = grouped
            return false
        }

        func textFieldDidBeginEditing(_ field: UITextField) { if !parent.focused { parent.focused = true } }
        func textFieldDidEndEditing(_ field: UITextField) { if parent.focused { parent.focused = false } }
    }
}
#endif

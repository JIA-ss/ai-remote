import SwiftUI

// Keep the storage wrapper unambiguous with SDKs that also expose a State macro.
typealias PhoneViewState<Value> = SwiftUI.State<Value>

struct PhoneViewportRequest: Equatable {
    enum Mode: Equatable { case fit, fill }
    var mode: Mode = .fit
    var revision = 0
}

struct PhonePointerGesture {
    let start: CGPoint
    let end: CGPoint
    let isDrag: Bool
}

#if os(iOS)
import UIKit

/// UIKit retains the view, recognizers and original pixel bounds across video frames.
struct PhoneRemoteViewport: UIViewRepresentable {
    let image: CGImage
    let allowsInput: Bool
    let request: PhoneViewportRequest
    let onGesture: (PhonePointerGesture) -> Void
    let onInsets: (UIEdgeInsets) -> Void
    /// The host's pointer to draw over the picture, with the screen rect its position is in.
    var pointer: PhonePointer? = nil
    var screen: ScreenGeometry? = nil
    #if DEBUG
    var diagnosticsEnabled = false
    #endif

    func makeUIView(context: Context) -> NativePhoneViewport { NativePhoneViewport() }

    func updateUIView(_ view: NativePhoneViewport, context: Context) {
        view.allowsRemoteInput = allowsInput
        view.onRemoteGesture = onGesture
        view.onInsets = onInsets
        #if DEBUG
        view.diagnosticsEnabled = diagnosticsEnabled
        #endif
        view.show(image, request: request)
        view.showPointer(pointer, screen: screen)
    }

    static func dismantleUIView(_ view: NativePhoneViewport, coordinator: ()) {
        view.allowsRemoteInput = false
        view.onRemoteGesture = nil
        view.onInsets = nil
        view.imageView.image = nil
        view.pointerView.image = nil
    }
}

final class NativePhoneViewport: UIScrollView, UIScrollViewDelegate, UIGestureRecognizerDelegate {
    let imageView = UIImageView()
    /// The host's pointer, a child of the picture so that it zooms and pans with it.
    let pointerView = UIImageView()
    private var lastPointerShape: UInt32?
    var onRemoteGesture: ((PhonePointerGesture) -> Void)?
    var onInsets: ((UIEdgeInsets) -> Void)?
    var allowsRemoteInput = false {
        didSet {
            guard oldValue != allowsRemoteInput else { return }
            remoteTap.isEnabled = allowsRemoteInput
            remoteDrag.isEnabled = allowsRemoteInput
            dragStart = nil
        }
    }
    #if DEBUG
    var diagnosticsEnabled = false
    // A 180-degree rotation need not change the view's bounds or run layout again.
    // Read scene orientation when accessibility asks, rather than reporting an old layout sample.
    override var accessibilityValue: String? {
        get {
            guard diagnosticsEnabled, let stored = super.accessibilityValue, let data = stored.data(using: .utf8),
                  var value = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else {
                return super.accessibilityValue
            }
            value["interface_orientation"] = window?.windowScene?.interfaceOrientation.rawValue ?? 0
            guard let data = try? JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]) else { return stored }
            return String(data: data, encoding: .utf8)
        }
        set { super.accessibilityValue = newValue }
    }
    #endif
    private var imageSize = CGSize.zero
    private var lastViewport = CGSize.zero
    private var lastAvailableRect = CGRect.zero
    private var lastImage: CGImage?
    private var lastRequest: PhoneViewportRequest?
    private var pendingRequest: PhoneViewportRequest?
    private var lastPublishedInsets: UIEdgeInsets?
    private var adjusting = false
    private var dragStart: CGPoint?
    private lazy var remoteTap = RemoteSingleTouchTapRecognizer(target: self, action: #selector(tapEnded(_:)))
    private lazy var remoteDrag = RemoteSingleTouchPanRecognizer(target: self, action: #selector(dragChanged(_:)))

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .black
        clipsToBounds = true
        delegate = self
        contentInsetAdjustmentBehavior = .never
        showsHorizontalScrollIndicator = false
        showsVerticalScrollIndicator = false
        bounces = false
        bouncesZoom = false
        delaysContentTouches = false
        panGestureRecognizer.minimumNumberOfTouches = 2
        panGestureRecognizer.maximumNumberOfTouches = 2
        imageView.contentMode = .scaleToFill
        imageView.isUserInteractionEnabled = false
        addSubview(imageView)
        pointerView.contentMode = .scaleToFill
        pointerView.isUserInteractionEnabled = false
        pointerView.isHidden = true
        imageView.addSubview(pointerView)
        remoteDrag.minimumNumberOfTouches = 1
        remoteDrag.maximumNumberOfTouches = 1
        remoteDrag.captureInitialImagePoint = { [weak self] touch in
            guard let self else { return nil }
            return PhoneViewportGeometry.normalizedPoint(touch.location(in: self.imageView), imageSize: self.imageSize)
        }
        remoteTap.numberOfTouchesRequired = 1
        remoteTap.permitsInitialTouch = { [weak self] touch in
            guard let self else { return false }
            return PhoneViewportGeometry.normalizedPoint(touch.location(in: self.imageView), imageSize: self.imageSize) != nil
        }
        remoteTap.require(toFail: remoteDrag)
        remoteTap.delegate = self
        remoteDrag.delegate = self
        remoteTap.isEnabled = false
        remoteDrag.isEnabled = false
        addGestureRecognizer(remoteTap)
        addGestureRecognizer(remoteDrag)
        isAccessibilityElement = true
        accessibilityIdentifier = "remote-viewport"
        accessibilityLabel = "远程电脑画面"
        accessibilityHint = "双指缩放和移动画面；接管后单指轻点或拖动操作电脑"
        accessibilityTraits = [.image, .allowsDirectInteraction]
        accessibilityCustomActions = [
            UIAccessibilityCustomAction(name: "适合画面", target: self, selector: #selector(performViewportFit)),
            UIAccessibilityCustomAction(name: "放大画面", target: self, selector: #selector(performViewportZoomIn)),
            UIAccessibilityCustomAction(name: "缩小画面", target: self, selector: #selector(performViewportZoomOut))
        ]
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    func show(_ image: CGImage, request: PhoneViewportRequest) {
        pendingRequest = request
        let size = CGSize(width: image.width, height: image.height)
        if lastImage !== image {
            // UIImage wraps this immutable CGImage; no bitmap drawing or pixel copy.
            imageView.image = UIImage(cgImage: image)
            lastImage = image
        }
        if imageSize != size {
            cancelPendingInput()
            adjusting = true
            minimumZoomScale = 0.0001
            maximumZoomScale = 100
            setZoomScale(1, animated: false)
            imageView.frame = CGRect(origin: .zero, size: size)
            contentSize = size
            imageSize = size
            lastViewport = .zero
            lastAvailableRect = .zero
            adjusting = false
            setNeedsLayout()
            layoutIfNeeded()
        }
        if lastRequest != request, bounds.width > 0, bounds.height > 0 {
            lastRequest = request
            apply(request.mode)
        }
    }

    /// Places the pointer picture in image pixels: the position is in the screen rect's units
    /// (host points), the picture in stream pixels. A pointer without a picture is a plain arrow.
    func showPointer(_ pointer: PhonePointer?, screen: ScreenGeometry?) {
        guard let pointer, pointer.visible, let screen, imageSize.width > 0, imageSize.height > 0 else {
            pointerView.isHidden = true
            return
        }
        let sx = imageSize.width / CGFloat(screen.width), sy = imageSize.height / CGFloat(screen.height)
        let (hotX, hotY, w, h): (CGFloat, CGFloat, CGFloat, CGFloat)
        if let shape = pointer.shape {
            if lastPointerShape != shape.id || pointerView.image == nil {
                pointerView.image = UIImage(cgImage: shape.image)
                lastPointerShape = shape.id
            }
            (hotX, hotY, w, h) = (CGFloat(shape.hotX), CGFloat(shape.hotY), CGFloat(shape.width), CGFloat(shape.height))
        } else {
            if lastPointerShape != 0 || pointerView.image == nil {
                pointerView.image = Self.defaultArrow
                lastPointerShape = 0
            }
            (hotX, hotY, w, h) = (0, 0, Self.defaultArrow.size.width, Self.defaultArrow.size.height)
        }
        pointerView.frame = CGRect(x: (CGFloat(pointer.x) - hotX) * sx, y: (CGFloat(pointer.y) - hotY) * sy, width: w * sx, height: h * sy)
        pointerView.isHidden = false
    }

    /// A black arrow with a white edge, 12x19 points, for shape 0.
    private static let defaultArrow: UIImage = {
        let size = CGSize(width: 12, height: 19)
        return UIGraphicsImageRenderer(size: size).image { _ in
            let path = UIBezierPath()
            path.move(to: CGPoint(x: 0.5, y: 0.5)); path.addLine(to: CGPoint(x: 0.5, y: 16.5))
            path.addLine(to: CGPoint(x: 4.5, y: 12.5)); path.addLine(to: CGPoint(x: 7, y: 18.5))
            path.addLine(to: CGPoint(x: 9.5, y: 17.5)); path.addLine(to: CGPoint(x: 7, y: 11.5))
            path.addLine(to: CGPoint(x: 11.5, y: 11.5)); path.close()
            UIColor.black.setFill(); path.fill()
            UIColor.white.setStroke(); path.lineWidth = 1; path.stroke()
        }
    }()

    override func layoutSubviews() {
        let oldCenter: CGPoint? = zoomScale > 0 && lastAvailableRect != .zero
            ? CGPoint(x: (contentOffset.x + lastAvailableRect.midX) / zoomScale,
                      y: (contentOffset.y + lastAvailableRect.midY) / zoomScale) : nil
        let relativeZoom = lastViewport != .zero && minimumZoomScale > 0 ? zoomScale / minimumZoomScale : 1
        super.layoutSubviews()
        let available = availableRect
        guard !adjusting, bounds.size != lastViewport || available != lastAvailableRect,
              let fit = PhoneViewportGeometry.fitScale(image: imageSize, viewport: available.size) else { return }
        cancelPendingInput()
        adjusting = true
        lastViewport = bounds.size
        lastAvailableRect = available
        publishInsets(for: available)
        maximumZoomScale = zoomLimit(fit: fit)
        minimumZoomScale = fit
        setZoomScale(min(max(fit * relativeZoom, fit), maximumZoomScale), animated: false)
        centerContent()
        let center = oldCenter ?? CGPoint(x: imageSize.width / 2, y: imageSize.height / 2)
        moveCenter(to: center)
        adjusting = false
        updateAccessibility()
        // The first representable update can precede UIKit's nonzero bounds.
        if let request = pendingRequest, lastRequest != request {
            lastRequest = request
            apply(request.mode)
        }
    }

    private func publishInsets(for available: CGRect) {
        let next = UIEdgeInsets(top: available.minY, left: available.minX,
                               bottom: bounds.height - available.maxY, right: bounds.width - available.maxX)
        guard next != lastPublishedInsets else { return }
        lastPublishedInsets = next
        DispatchQueue.main.async { [weak self] in
            guard let self, self.lastPublishedInsets == next else { return }
            self.onInsets?(next)
        }
    }

    override func safeAreaInsetsDidChange() {
        super.safeAreaInsetsDidChange()
        setNeedsLayout()
    }

    /// Fullscreen paints the background to the edges, while fit keeps every source
    /// pixel inside the notch/rounded-corner safe area. An already inset view adds zero.
    private var availableRect: CGRect {
        var inset = safeAreaInsets
        if let window {
            // SwiftUI may override the representable's inherited insets when it
            // ignores container safe areas. Intersect the actual UIWindow safe area.
            let safe = convert(window.bounds.inset(by: window.safeAreaInsets), from: window).intersection(bounds)
            if !safe.isNull {
                inset.left = max(inset.left, safe.minX - bounds.minX)
                inset.top = max(inset.top, safe.minY - bounds.minY)
                inset.right = max(inset.right, bounds.maxX - safe.maxX)
                inset.bottom = max(inset.bottom, bounds.maxY - safe.maxY)
            }
        }
        return CGRect(x: max(0, inset.left), y: max(0, inset.top),
                      width: max(0, bounds.width - max(0, inset.left) - max(0, inset.right)),
                      height: max(0, bounds.height - max(0, inset.top) - max(0, inset.bottom)))
    }

    func viewForZooming(in scrollView: UIScrollView) -> UIView? { imageView }
    func scrollViewDidZoom(_ scrollView: UIScrollView) {
        guard !adjusting else { return }
        centerContent()
        updateAccessibility()
    }
    func scrollViewDidScroll(_ scrollView: UIScrollView) {
        guard !adjusting else { return }
        updateAccessibility()
    }

    private func centerContent() {
        let available = availableRect
        let scaled = CGSize(width: imageSize.width * zoomScale, height: imageSize.height * zoomScale)
        guard let inset = PhoneViewportGeometry.centeringOffset(contentSize: scaled, viewport: available.size) else { return }
        let next = UIEdgeInsets(top: available.minY + inset.y, left: available.minX + inset.x,
                               bottom: bounds.height - available.maxY + inset.y,
                               right: bounds.width - available.maxX + inset.x)
        if contentInset != next { contentInset = next }
    }

    private func moveCenter(to pixel: CGPoint) {
        let available = availableRect
        let scaled = CGSize(width: imageSize.width * zoomScale, height: imageSize.height * zoomScale)
        let desired = CGPoint(x: pixel.x * zoomScale - available.width / 2,
                              y: pixel.y * zoomScale - available.height / 2)
        guard let inset = PhoneViewportGeometry.centeringOffset(contentSize: scaled, viewport: available.size),
              let offset = PhoneViewportGeometry.clampOffset(desired, contentSize: scaled, viewport: available.size, centering: inset) else { return }
        setContentOffset(CGPoint(x: offset.x - available.minX, y: offset.y - available.minY), animated: false)
    }

    private func apply(_ mode: PhoneViewportRequest.Mode) {
        guard let fit = PhoneViewportGeometry.fitScale(image: imageSize, viewport: availableRect.size) else { return }
        cancelPendingInput()
        adjusting = true
        maximumZoomScale = zoomLimit(fit: fit)
        minimumZoomScale = fit
        let fill = max(bounds.width / imageSize.width, bounds.height / imageSize.height)
        setZoomScale(mode == .fit ? fit : min(fill, maximumZoomScale), animated: false)
        centerContent()
        moveCenter(to: CGPoint(x: imageSize.width / 2, y: imageSize.height / 2))
        adjusting = false
        updateAccessibility()
    }

    private func cancelPendingInput() {
        dragStart = nil
        // Also revoke a possible tap/drag: its old touch point must not be reused
        // against a new image size, rotation or fit transform.
        for recognizer in [remoteTap as UIGestureRecognizer, remoteDrag] {
            recognizer.isEnabled = false
            recognizer.isEnabled = allowsRemoteInput
        }
    }

    private func zoomLimit(fit: CGFloat) -> CGFloat {
        // Even an ultrawide desktop can fill a portrait viewport and then be panned.
        max(fit * 6, max(bounds.width / imageSize.width, bounds.height / imageSize.height))
    }

    private func point(_ recognizer: UIGestureRecognizer) -> CGPoint? {
        PhoneViewportGeometry.normalizedPoint(recognizer.location(in: imageView), imageSize: imageSize)
    }

    func gestureRecognizer(_ gestureRecognizer: UIGestureRecognizer, shouldReceive touch: UITouch) -> Bool {
        guard gestureRecognizer === remoteTap || gestureRecognizer === remoteDrag else { return true }
        guard allowsRemoteInput else { return false }
        // Both recognizers must see additional fingers even in letterboxing.
        // Initial-image bounds are checked separately, without hiding a second touch.
        return true
    }
    override func gestureRecognizerShouldBegin(_ gestureRecognizer: UIGestureRecognizer) -> Bool {
        guard gestureRecognizer === remoteTap || gestureRecognizer === remoteDrag else {
            return super.gestureRecognizerShouldBegin(gestureRecognizer)
        }
        return allowsRemoteInput && !isZooming && !isDecelerating && point(gestureRecognizer) != nil
    }

    func gestureRecognizer(_ gestureRecognizer: UIGestureRecognizer,
                           shouldRecognizeSimultaneouslyWith otherGestureRecognizer: UIGestureRecognizer) -> Bool {
        let native = otherGestureRecognizer === panGestureRecognizer || otherGestureRecognizer === pinchGestureRecognizer
        let nativeSource = gestureRecognizer === panGestureRecognizer || gestureRecognizer === pinchGestureRecognizer
        return native && (gestureRecognizer === remoteDrag || nativeSource)
    }

    @objc private func tapEnded(_ recognizer: RemoteSingleTouchTapRecognizer) {
        guard recognizer.state == .ended, allowsRemoteInput, !isZooming,
              recognizer.startedInsideImage, !recognizer.involvedMultipleTouches,
              let location = point(recognizer) else { return }
        deliver(PhonePointerGesture(start: location, end: location, isDrag: false), multipleTouches: recognizer.involvedMultipleTouches)
    }

    @objc private func dragChanged(_ recognizer: RemoteSingleTouchPanRecognizer) {
        switch recognizer.state {
        case .began:
            // UIPan's translation may start after its recognition hysteresis.
            // Use the actual touch-down position captured before that threshold.
            dragStart = allowsRemoteInput && !isZooming ? recognizer.initialImagePoint : nil
        case .ended:
            defer { dragStart = nil }
            guard allowsRemoteInput, !isZooming, !recognizer.involvedMultipleTouches,
                  let start = dragStart, let end = point(recognizer) else { return }
            let translation = recognizer.translation(in: self)
            let drag = hypot(translation.x, translation.y) > 10
            deliver(PhonePointerGesture(start: start, end: end, isDrag: drag), multipleTouches: recognizer.involvedMultipleTouches)
        case .cancelled, .failed: dragStart = nil
        default: break
        }
    }

    /// Stable completion policy, shared by real tap/pan endings and native tests.
    /// UIKit may reset instantaneous recognizer states after cancellation.
    @discardableResult func deliver(_ gesture: PhonePointerGesture, multipleTouches: Bool) -> Bool {
        guard allowsRemoteInput, !isZooming, !multipleTouches else { return false }
        onRemoteGesture?(gesture)
        return true
    }

    @objc private func performViewportFit() -> Bool { apply(.fit); return true }
    @objc private func performViewportZoomIn() -> Bool { accessibilityZoom(by: 1.5) }
    @objc private func performViewportZoomOut() -> Bool { accessibilityZoom(by: 1 / 1.5) }
    private func accessibilityZoom(by factor: CGFloat) -> Bool {
        setZoomScale(min(max(zoomScale * factor, minimumZoomScale), maximumZoomScale), animated: false)
        return true
    }

    private func updateAccessibility() {
        accessibilityValue = "缩放 \(Int((zoomScale / max(minimumZoomScale, 0.0001) * 100).rounded()))%"
        #if DEBUG
        if diagnosticsEnabled {
            let displayed = imageView.convert(imageView.bounds, to: self)
            let available = availableRect
            let value: [String: Double] = [
                "interface_orientation": Double(window?.windowScene?.interfaceOrientation.rawValue ?? 0),
                "viewport_width": bounds.width, "viewport_height": bounds.height,
                "available_x": available.minX, "available_y": available.minY,
                "available_width": available.width, "available_height": available.height,
                "image_width": imageSize.width, "image_height": imageSize.height,
                "displayed_x": displayed.minX - bounds.minX, "displayed_y": displayed.minY - bounds.minY,
                "displayed_width": displayed.width, "displayed_height": displayed.height,
                "zoom": zoomScale, "fit": minimumZoomScale,
                "offset_x": contentOffset.x, "offset_y": contentOffset.y
            ]
            if let data = try? JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]),
               let text = String(data: data, encoding: .utf8) { accessibilityValue = text }
        }
        #endif
    }
}

/// UIKit's maximumNumberOfTouches alone does not revoke a pan that already began.
/// Cancel explicitly on a second finger, without sending any remote down/up events.
class RemoteSingleTouchPanRecognizer: UIPanGestureRecognizer {
    var captureInitialImagePoint: ((UITouch) -> CGPoint?)?
    private(set) var initialImagePoint: CGPoint?
    private(set) var involvedMultipleTouches = false

    @discardableResult func activeTouchCountChanged(_ count: Int) -> Bool {
        guard count > 1 else { return false }
        involvedMultipleTouches = true
        if state == .began || state == .changed { state = .cancelled }
        else if state == .possible { state = .failed }
        return true
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent) {
        activeTouchCountChanged(event.allTouches?.filter { $0.phase != .ended && $0.phase != .cancelled }.count ?? touches.count)
        guard !involvedMultipleTouches else { return }
        if let first = touches.first { initialImagePoint = captureInitialImagePoint?(first) }
        super.touchesBegan(touches, with: event)
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent) {
        activeTouchCountChanged(event.allTouches?.filter { $0.phase != .ended && $0.phase != .cancelled }.count ?? numberOfTouches)
        guard !involvedMultipleTouches else { return }
        super.touchesMoved(touches, with: event)
    }

    override func reset() {
        super.reset()
        involvedMultipleTouches = false
        initialImagePoint = nil
    }
}

class RemoteSingleTouchTapRecognizer: UITapGestureRecognizer {
    var permitsInitialTouch: ((UITouch) -> Bool)?
    private(set) var startedInsideImage = false
    private(set) var involvedMultipleTouches = false

    @discardableResult func activeTouchCountChanged(_ count: Int) -> Bool {
        guard count > 1 else { return false }
        involvedMultipleTouches = true
        if state == .began || state == .changed { state = .cancelled }
        else if state == .possible { state = .failed }
        return true
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent) {
        activeTouchCountChanged(event.allTouches?.filter { $0.phase != .ended && $0.phase != .cancelled }.count ?? touches.count)
        guard !involvedMultipleTouches, let first = touches.first else { return }
        startedInsideImage = permitsInitialTouch?(first) ?? false
        guard startedInsideImage else { state = .failed; return }
        super.touchesBegan(touches, with: event)
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent) {
        activeTouchCountChanged(event.allTouches?.filter { $0.phase != .ended && $0.phase != .cancelled }.count ?? numberOfTouches)
        guard !involvedMultipleTouches else { return }
        super.touchesMoved(touches, with: event)
    }

    override func reset() {
        super.reset()
        involvedMultipleTouches = false
        startedInsideImage = false
    }
}
#endif

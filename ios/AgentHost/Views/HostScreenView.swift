import SwiftUI
import UIKit

/// The host's screen, full screen: pick what to show, then watch (and, when
/// the desktop allows it, tap to click and type). For the moments a task
/// needs the computer itself: a sign-in page, an approval in a browser.
struct HostScreenView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    @StateObject private var stream = ScreenStream()
    @State private var mode = ScreenModeName.desktop
    @State private var control = false
    @State private var keyboard = false
    @State private var chromeHidden = false
    /// Demo screenshots: start streaming at once.
    var startImmediately = false

    private var status: ScreenStatusModel? { model.screenStatus }

    var body: some View {
        Group {
            switch stream.phase {
            case .idle:
                picker
            case .connecting where stream.size == .zero:
                connecting
            case let .ended(reason) where stream.size == .zero:
                ended(reason)
            default:
                viewer
            }
        }
        .preferredColorScheme(.dark)
        .onAppear {
            if let first = status?.modes.first(where: \.available) { mode = first.mode }
            control = status?.policy.allowControl ?? false
            if startImmediately { stream.start(model: model, mode: mode, control: control) }
        }
        .onDisappear { stream.close() }
    }

    // MARK: Picker

    private var picker: some View {
        NavigationStack {
            List {
                if let status, status.policy.enabled {
                    Section {
                        ForEach(status.modes) { m in
                            Button {
                                mode = m.mode
                            } label: {
                                HStack(spacing: 12) {
                                    SettingsIcon(systemName: m.systemImage, color: m.available ? .blue : Color(white: 0.35))
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text(m.title).foregroundStyle(m.available ? Color.primary : Color.secondary)
                                        Text(m.detail).font(.footnote).foregroundStyle(Color(uiColor: .secondaryLabel))
                                    }
                                    Spacer(minLength: 8)
                                    if mode == m.mode && m.available {
                                        Image(systemName: "checkmark").foregroundStyle(.tint).fontWeight(.semibold)
                                    }
                                }
                            }
                            .disabled(!m.available)
                        }
                    } header: {
                        GroupHeader("Show")
                    } footer: {
                        Text("Browser on Host opens a separate desktop with only a browser: it works while the computer is locked, and sign-ins there reach the host's own sign-in callbacks.")
                    }
                    Section {
                        Toggle(isOn: $control) {
                            HStack(spacing: 12) {
                                SettingsIcon(systemName: "hand.tap.fill", color: .orange)
                                Text("Click and Type")
                            }
                        }
                        .disabled(!status.policy.allowControl)
                    } footer: {
                        Text(status.policy.allowControl
                             ? "Tap to click, touch and hold to right-click, drag with one finger to scroll, pinch to zoom."
                             : "This host allows watching only. Allow control on the computer: Settings → Screen Sharing.")
                    }
                    if let e = status.error {
                        Section { Text(e).font(.footnote).foregroundStyle(Palette.orange) }
                    }
                } else {
                    ContentUnavailableView {
                        Label("Screen Sharing Is Off", systemImage: "display")
                    } description: {
                        Text("Turn it on on the computer: Settings → Screen Sharing. Your phone can then see the host's screen when a task needs it.")
                    }
                    .listRowBackground(Color.clear)
                }
            }
            .listStyle(.insetGrouped)
            .navigationTitle("Host Screen")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("View") {
                        stream.start(model: model, mode: mode, control: control && (status?.policy.allowControl ?? false))
                    }
                    .fontWeight(.semibold)
                    .disabled(!(status?.policy.enabled ?? false) || !(status?.modes.contains { $0.mode == mode && $0.available } ?? false))
                }
            }
            .task { await model.refreshScreen() }
        }
    }

    private var connecting: some View {
        VStack(spacing: 14) {
            ProgressView().controlSize(.large)
            Text("Starting screen sharing on \(model.hostName)…").foregroundStyle(.secondary)
            Button("Cancel") { stream.close() }.padding(.top, 8)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.black)
    }

    private func ended(_ reason: String) -> some View {
        NavigationStack {
            ContentUnavailableView {
                Label("Couldn't Show the Screen", systemImage: "exclamationmark.triangle")
            } description: {
                Text(reason)
            } actions: {
                Button("Try Again") { stream.start(model: model, mode: mode, control: control) }
                    .buttonStyle(.borderedProminent)
                Button("Back") { stream.close(); resetToPicker() }
            }
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Close") { dismiss() } }
            }
        }
    }

    private func resetToPicker() {
        stream.close()
    }

    // MARK: Viewer

    private var viewer: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            ScreenCanvas(
                renderer: stream.renderer,
                desktop: stream.size,
                control: stream.control,
                onTap: { p in stream.click(p) },
                onLongPress: { p in stream.rightClick(p) },
                onScroll: { p, n in stream.scroll(p, notches: n) },
                onToggleChrome: { withAnimation(.easeOut(duration: 0.2)) { chromeHidden.toggle() } }
            )
            .ignoresSafeArea()
            KeyCapture(active: $keyboard, onText: stream.type, onKey: { stream.key($0) })
                .frame(width: 1, height: 1)
                .opacity(0.01)
        }
        .overlay(alignment: .top) {
            if !chromeHidden { topBar.transition(.move(edge: .top).combined(with: .opacity)) }
        }
        .overlay(alignment: .bottom) {
            if stream.control && !chromeHidden { keyBar.transition(.move(edge: .bottom).combined(with: .opacity)) }
        }
        .overlay {
            if case let .ended(reason) = stream.phase {
                VStack(spacing: 12) {
                    Text("The stream ended").font(.headline)
                    Text(reason).font(.footnote).foregroundStyle(.secondary).multilineTextAlignment(.center)
                    Button("Reconnect") { stream.start(model: model, mode: mode, control: control) }
                        .buttonStyle(.borderedProminent)
                }
                .padding(20)
                .glassSurface(RoundedRectangle(cornerRadius: 22, style: .continuous))
                .padding(32)
            }
        }
        .statusBarHidden(chromeHidden)
        .onAppear {
            OrientationLock.set(.allButUpsideDown)
            #if DEBUG
            if DemoData.landscape {
                // After the cover is on screen; asking earlier is ignored.
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
                    let scene = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first
                    scene?.requestGeometryUpdate(.iOS(interfaceOrientations: .landscapeRight))
                }
            }
            #endif
        }
        .onDisappear { OrientationLock.set(.portrait) }
    }

    private var title: String {
        stream.mode == ScreenModeName.browser ? "Browser on \(model.hostName)" : model.hostName
    }

    private var topBar: some View {
        HStack(spacing: 10) {
            Button {
                stream.close()
                dismiss()
            } label: {
                ToolbarIcon(systemName: "xmark")
            }
            .accessibilityLabel("Close")
            VStack(alignment: .leading, spacing: 1) {
                Text(title).font(.subheadline.weight(.semibold)).lineLimit(1)
                HStack(spacing: 6) {
                    Circle().fill(stream.isLive ? Palette.green : Palette.orange).frame(width: 7, height: 7)
                    Text(subtitle).font(.caption).foregroundStyle(.secondary).monospacedDigit()
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 7)
            .glassSurface(Capsule())
            Spacer(minLength: 0)
            Menu {
                Button(role: .destructive) {
                    stream.close()
                    Task { await model.stopScreen() }
                    dismiss()
                } label: {
                    Label("Stop Sharing on Host", systemImage: "stop.circle")
                }
            } label: {
                ToolbarIcon(systemName: "ellipsis")
            }
            .accessibilityLabel("More")
        }
        .padding(.horizontal, 16)
        .padding(.top, 6)
        .glassGroup()
    }

    private var subtitle: String {
        let what = stream.control ? "Control" : "View only"
        guard stream.size != .zero else { return what }
        return "\(what) · \(Int(stream.size.width))×\(Int(stream.size.height)) · \(stream.framesPerSecond) fps"
    }

    private var keyBar: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                KeyButton(systemName: keyboard ? "keyboard.chevron.compact.down" : "keyboard", label: "Keyboard", prominent: keyboard) {
                    keyboard.toggle()
                }
                KeyButton(text: "esc", label: "Escape") { stream.key(HID.escape) }
                KeyButton(text: "tab", label: "Tab") { stream.key(HID.tab) }
                KeyButton(systemName: "return", label: "Return") { stream.key(HID.enter) }
                KeyButton(systemName: "delete.left", label: "Delete") { stream.key(HID.backspace) }
                KeyButton(systemName: "arrow.left", label: "Left") { stream.key(HID.left) }
                KeyButton(systemName: "arrow.up", label: "Up") { stream.key(HID.up) }
                KeyButton(systemName: "arrow.down", label: "Down") { stream.key(HID.down) }
                KeyButton(systemName: "arrow.right", label: "Right") { stream.key(HID.right) }
                KeyButton(text: model.hostInfo?.os.lowercased() == "macos" ? "⌘V" : "ctrl V", label: "Paste") {
                    stream.key(HID.v, modifiers: [model.hostInfo?.os.lowercased() == "macos" ? HID.leftGUI : HID.leftControl])
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 6)
        }
        .glassGroup(spacing: 8)
        .padding(.bottom, 4)
    }
}

/// HID usages for the keys the bar sends.
enum HID {
    static let v: UInt16 = 25
    static let enter: UInt16 = 40
    static let escape: UInt16 = 41
    static let backspace: UInt16 = 42
    static let tab: UInt16 = 43
    static let right: UInt16 = 79
    static let left: UInt16 = 80
    static let down: UInt16 = 81
    static let up: UInt16 = 82
    static let leftControl: UInt16 = 224
    static let leftGUI: UInt16 = 227
}

private struct KeyButton: View {
    var systemName: String?
    var text: String?
    let label: String
    var prominent = false
    let action: () -> Void

    init(systemName: String, label: String, prominent: Bool = false, action: @escaping () -> Void) {
        self.systemName = systemName
        self.label = label
        self.prominent = prominent
        self.action = action
    }

    init(text: String, label: String, action: @escaping () -> Void) {
        self.text = text
        self.label = label
        self.action = action
    }

    var body: some View {
        Button(action: action) {
            Group {
                if let systemName {
                    Image(systemName: systemName).font(.body.weight(.medium))
                } else if let text {
                    Text(text).font(.subheadline.weight(.medium))
                }
            }
            .foregroundStyle(Color.primary)
            .frame(minWidth: 44, minHeight: 44)
            .padding(.horizontal, text == nil ? 0 : 6)
            .glassSurface(Capsule(), tint: prominent ? Color.accentColor.opacity(0.6) : nil, interactive: true)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
    }
}

// MARK: - Canvas

/// The desktop, fitted to the screen. Pinch or pan with two fingers to zoom
/// and move around; tap to click, touch and hold to right-click, drag with
/// one finger to scroll (when controlling). A tap without control shows or
/// hides the bars.
struct ScreenCanvas: UIViewRepresentable {
    let renderer: H264Renderer
    let desktop: CGSize
    let control: Bool
    let onTap: (CGPoint) -> Void
    let onLongPress: (CGPoint) -> Void
    let onScroll: (CGPoint, Int32) -> Void
    let onToggleChrome: () -> Void

    func makeUIView(context: Context) -> ScreenCanvasView {
        ScreenCanvasView(renderer: renderer)
    }

    func updateUIView(_ view: ScreenCanvasView, context: Context) {
        view.desktop = desktop
        view.control = control
        view.onTap = onTap
        view.onLongPress = onLongPress
        view.onScroll = onScroll
        view.onToggleChrome = onToggleChrome
    }
}

final class ScreenCanvasView: UIView, UIScrollViewDelegate {
    private let scroll = UIScrollView()
    private let content = UIView()
    private let renderer: H264Renderer
    var control = false
    var onTap: (CGPoint) -> Void = { _ in }
    var onLongPress: (CGPoint) -> Void = { _ in }
    var onScroll: (CGPoint, Int32) -> Void = { _, _ in }
    var onToggleChrome: () -> Void = {}
    var desktop: CGSize = .zero {
        didSet { if desktop != oldValue { setNeedsLayout() } }
    }
    private var fittedFor: CGSize = .zero
    private var scrollCarry: CGFloat = 0
    /// Points of finger travel per wheel notch.
    private let pointsPerNotch: CGFloat = 36

    init(renderer: H264Renderer) {
        self.renderer = renderer
        super.init(frame: .zero)
        backgroundColor = .black
        scroll.delegate = self
        scroll.minimumZoomScale = 1
        scroll.maximumZoomScale = 6
        scroll.showsHorizontalScrollIndicator = false
        scroll.showsVerticalScrollIndicator = false
        scroll.contentInsetAdjustmentBehavior = .never
        scroll.bouncesZoom = true
        // One finger belongs to the desktop (scroll); two move the view.
        scroll.panGestureRecognizer.minimumNumberOfTouches = 2
        addSubview(scroll)
        scroll.addSubview(content)
        content.layer.addSublayer(renderer.layer)

        let tap = UITapGestureRecognizer(target: self, action: #selector(tapped(_:)))
        let hold = UILongPressGestureRecognizer(target: self, action: #selector(held(_:)))
        hold.minimumPressDuration = 0.5
        let drag = UIPanGestureRecognizer(target: self, action: #selector(dragged(_:)))
        drag.maximumNumberOfTouches = 1
        tap.require(toFail: hold)
        let zoomTap = UITapGestureRecognizer(target: self, action: #selector(zoomTapped(_:)))
        zoomTap.numberOfTouchesRequired = 2
        for g in [tap, hold, drag, zoomTap] { content.addGestureRecognizer(g) }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    override func layoutSubviews() {
        super.layoutSubviews()
        scroll.frame = bounds
        guard desktop.width > 0, desktop.height > 0, bounds.width > 0 else { return }
        let key = CGSize(width: bounds.width * 10_000 + desktop.width, height: bounds.height * 10_000 + desktop.height)
        if key != fittedFor {
            fittedFor = key
            scroll.zoomScale = 1
            let s = min(bounds.width / desktop.width, bounds.height / desktop.height)
            let fitted = CGSize(width: desktop.width * s, height: desktop.height * s)
            content.frame = CGRect(origin: .zero, size: fitted)
            scroll.contentSize = fitted
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            renderer.layer.frame = content.bounds
            CATransaction.commit()
        }
        center()
    }

    private func center() {
        let w = max(0, (scroll.bounds.width - content.frame.width) / 2)
        let h = max(0, (scroll.bounds.height - content.frame.height) / 2)
        scroll.contentInset = UIEdgeInsets(top: h, left: w, bottom: h, right: w)
    }

    func viewForZooming(in scrollView: UIScrollView) -> UIView? { content }
    func scrollViewDidZoom(_ scrollView: UIScrollView) { center() }

    /// The touch as a fraction of the desktop.
    private func fraction(_ g: UIGestureRecognizer) -> CGPoint {
        let p = g.location(in: content)
        let b = content.bounds
        return CGPoint(x: min(max(p.x / b.width, 0), 1), y: min(max(p.y / b.height, 0), 1))
    }

    @objc private func tapped(_ g: UITapGestureRecognizer) {
        guard control else { return onToggleChrome() }
        onTap(fraction(g))
        ripple(at: g.location(in: self), color: .white)
    }

    @objc private func held(_ g: UILongPressGestureRecognizer) {
        guard control, g.state == .began else { return }
        UIImpactFeedbackGenerator(style: .medium).impactOccurred()
        onLongPress(fraction(g))
        ripple(at: g.location(in: self), color: .systemOrange)
    }

    @objc private func dragged(_ g: UIPanGestureRecognizer) {
        guard control else { return }
        if g.state == .began { scrollCarry = 0 }
        let dy = g.translation(in: self).y
        g.setTranslation(.zero, in: self)
        // Content follows the finger: dragging up scrolls down.
        scrollCarry -= dy
        let notches = Int32(scrollCarry / pointsPerNotch)
        if notches != 0 {
            scrollCarry -= CGFloat(notches) * pointsPerNotch
            onScroll(fraction(g), notches)
        }
    }

    @objc private func zoomTapped(_ g: UITapGestureRecognizer) {
        if scroll.zoomScale > 1.01 {
            scroll.setZoomScale(1, animated: true)
        } else {
            let p = g.location(in: content)
            let size = CGSize(width: content.bounds.width / 2.5, height: content.bounds.height / 2.5)
            scroll.zoom(to: CGRect(x: p.x - size.width / 2, y: p.y - size.height / 2, width: size.width, height: size.height), animated: true)
        }
    }

    /// Where a click landed, so a tap on a small target is visible.
    private func ripple(at point: CGPoint, color: UIColor) {
        let dot = UIView(frame: CGRect(x: 0, y: 0, width: 34, height: 34))
        dot.center = point
        dot.layer.cornerRadius = 17
        dot.layer.borderWidth = 2
        dot.layer.borderColor = color.withAlphaComponent(0.9).cgColor
        dot.backgroundColor = color.withAlphaComponent(0.18)
        dot.isUserInteractionEnabled = false
        addSubview(dot)
        UIView.animate(withDuration: 0.45, delay: 0, options: .curveEaseOut) {
            dot.transform = CGAffineTransform(scaleX: 1.6, y: 1.6)
            dot.alpha = 0
        } completion: { _ in
            dot.removeFromSuperview()
        }
    }
}

// MARK: - Keyboard

/// An invisible text input: the system keyboard types into the host.
struct KeyCapture: UIViewRepresentable {
    @Binding var active: Bool
    let onText: (String) -> Void
    let onKey: (UInt16) -> Void

    func makeUIView(context: Context) -> KeyCaptureView {
        let v = KeyCaptureView()
        v.onResign = { DispatchQueue.main.async { active = false } }
        return v
    }

    func updateUIView(_ v: KeyCaptureView, context: Context) {
        v.onText = onText
        v.onKey = onKey
        if active, !v.isFirstResponder {
            DispatchQueue.main.async { v.becomeFirstResponder() }
        } else if !active, v.isFirstResponder {
            DispatchQueue.main.async { _ = v.resignFirstResponder() }
        }
    }
}

final class KeyCaptureView: UIView, UIKeyInput {
    var onText: (String) -> Void = { _ in }
    var onKey: (UInt16) -> Void = { _ in }
    var onResign: () -> Void = {}

    var autocorrectionType: UITextAutocorrectionType = .no
    var autocapitalizationType: UITextAutocapitalizationType = .none
    var spellCheckingType: UITextSpellCheckingType = .no
    var smartQuotesType: UITextSmartQuotesType = .no
    var smartDashesType: UITextSmartDashesType = .no
    var keyboardAppearance: UIKeyboardAppearance = .dark

    override var canBecomeFirstResponder: Bool { true }
    var hasText: Bool { true }

    func insertText(_ text: String) {
        // Return and Tab arrive as characters; screen-relay types them as keys.
        onText(text)
    }

    func deleteBackward() { onKey(HID.backspace) }

    override func resignFirstResponder() -> Bool {
        let r = super.resignFirstResponder()
        onResign()
        return r
    }
}

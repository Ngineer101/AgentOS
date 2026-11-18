import AVFoundation
import SwiftUI
import UIKit

/// Forwards the confirmation code from the Rust layer to the main thread.
final class ShortCodeBridge: PairingListener, @unchecked Sendable {
    private let handler: @Sendable (String) -> Void

    init(_ handler: @escaping @Sendable (String) -> Void) {
        self.handler = handler
    }

    func onShortCode(code: String) {
        handler(code)
    }
}

@MainActor
final class PairFlow: ObservableObject {
    enum Step: Equatable {
        case scan
        case pairing(host: String)
        case confirm(host: String, code: String)
        case failed(String)
    }

    @Published var step: Step = .scan
    @Published var scanError: String?
    private var task: Task<Void, Never>?

    deinit {
        // The Pair screen went away (for example the app locked): stop pairing.
        task?.cancel()
    }

    func begin(qr: String, deviceName: String, model: AppModel) {
        guard step == .scan else { return }
        let info: PairingQrInfo
        do {
            info = try parsePairingQr(qr: qr)
        } catch {
            scanError = "That is not an AgentHost pairing code."
            return
        }
        scanError = nil
        step = .pairing(host: info.hostName)
        let host = info.hostName
        let listener = ShortCodeBridge { [weak self] code in
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    self?.step = .confirm(host: host, code: code)
                }
            }
        }
        let name = deviceName.trimmingCharacters(in: .whitespacesAndNewlines)
        task = Task { [weak self] in
            do {
                let creds = try await pair(
                    qr: qr,
                    deviceName: name.isEmpty ? "iPhone" : name,
                    listener: listener
                )
                try model.didPair(creds)
            } catch {
                guard !Task.isCancelled else { return }
                self?.step = .failed(userMessage(for: error))
            }
        }
    }

    func cancel() {
        task?.cancel()
        task = nil
        step = .scan
    }

    func reset() {
        task = nil
        step = .scan
    }
}

/// Pair screen: the camera fills the screen with a viewfinder; a panel at
/// the bottom explains the steps and offers Paste Code. Then compare the
/// confirmation code.
struct PairView: View {
    @EnvironmentObject private var model: AppModel
    @StateObject private var flow = PairFlow()
    @State private var pasted = ""
    @State private var deviceName = ""
    @State private var cameraAllowed: Bool?
    @State private var showPaste = false
    @State private var renaming = false
    @State private var newName = ""
    @Environment(\.openURL) private var openURL

    var body: some View {
        Group {
            if flow.step == .scan {
                scanStep
            } else {
                NavigationStack {
                    Group {
                        switch flow.step {
                        case .scan:
                            EmptyView()
                        case let .pairing(host):
                            progressStep(host: host)
                        case let .confirm(host, code):
                            confirmStep(host: host, code: code)
                        case let .failed(message):
                            failedStep(message)
                        }
                    }
                    .navigationTitle("Pair with your computer")
                    .navigationBarTitleDisplayMode(.inline)
                    .toolbar {
                        if isInProgress {
                            ToolbarItem(placement: .cancellationAction) {
                                Button("Cancel") { flow.cancel() }
                            }
                        }
                    }
                }
            }
        }
        .task {
            if deviceName.isEmpty { deviceName = UIDevice.current.name }
            #if DEBUG
            if model.isDemo {
                deviceName = "Nico's iPhone"
                return
            }
            #endif
            await checkCamera()
        }
        .sheet(isPresented: $showPaste) { pasteSheet }
        .alert("Name This iPhone", isPresented: $renaming) {
            TextField("Name", text: $newName)
                .textInputAutocapitalization(.words)
            Button("Cancel", role: .cancel) {}
            Button("Save") {
                let name = newName.trimmingCharacters(in: .whitespacesAndNewlines)
                if !name.isEmpty { deviceName = name }
            }
        } message: {
            Text("The computer lists this iPhone by this name.")
        }
    }

    private var isInProgress: Bool {
        switch flow.step {
        case .pairing, .confirm: return true
        default: return false
        }
    }

    // MARK: Scan

    private var scanStep: some View {
        ZStack {
            camera
                .ignoresSafeArea()

            VStack(spacing: 0) {
            VStack(spacing: 22) {
                Spacer(minLength: 0)
                Viewfinder()
                    .frame(width: 240, height: 240)
                if cameraAllowed == false {
                    VStack(spacing: 10) {
                        Text("Camera access is off. Allow it in Settings, or paste the code.")
                            .multilineTextAlignment(.center)
                            .foregroundStyle(.secondary)
                        Button("Open Settings") {
                            if let url = URL(string: UIApplication.openSettingsURLString) { openURL(url) }
                        }
                    }
                    .padding(.horizontal, 40)
                } else {
                    Text("Point at the code on your computer")
                        .foregroundStyle(.white.opacity(0.85))
                }
                Spacer(minLength: 0)
            }
            .frame(maxHeight: .infinity)

            panel
            }
        }
        .background(Color.black)
    }

    @ViewBuilder
    private var camera: some View {
        if cameraAllowed == true {
            QRScannerView { code in
                flow.begin(qr: code, deviceName: deviceName, model: model)
            }
        } else {
            RadialGradient(
                colors: [Color(white: 0.2), Color(white: 0.06)],
                center: UnitPoint(x: 0.5, y: 0.3), startRadius: 10, endRadius: 420
            )
        }
    }

    private var panel: some View {
        VStack(alignment: .leading, spacing: 16) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Pair with your computer")
                    .font(.title2.weight(.bold))
                Text("One time, takes about a minute")
                    .foregroundStyle(.secondary)
            }
            VStack(alignment: .leading, spacing: 12) {
                PairStep(number: 1, text: Text("On the computer, open AgentHost → Settings → Devices → \(Text("Pair iPhone").foregroundColor(.white))."))
                PairStep(number: 2, text: Text("Scan the code, then check both screens show the same six digits."))
            }
            if let err = flow.scanError {
                Label(err, systemImage: "exclamationmark.triangle.fill")
                    .font(.subheadline)
                    .foregroundStyle(Palette.orange)
            }
            Button {
                showPaste = true
            } label: {
                Label("Paste Code", systemImage: "doc.on.clipboard")
            }
            .buttonStyle(CapsuleButtonStyle(fill: Color.white.opacity(0.12), height: 50))
            .padding(.top, 2)

            HStack(spacing: 4) {
                Text("This iPhone will be called “\(deviceName)” ·")
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
                Button("Change") {
                    newName = deviceName
                    renaming = true
                }
            }
            .font(.caption)
            .frame(maxWidth: .infinity)
        }
        .padding(.horizontal, 24)
        .padding(.top, 28)
        .padding(.bottom, 8)
        .background(
            UnevenRoundedRectangle(topLeadingRadius: 38, topTrailingRadius: 38, style: .continuous)
                .fill(Palette.cell)
                .ignoresSafeArea(edges: .bottom)
        )
    }

    private var pasteSheet: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("agenthost://pair?…", text: $pasted, axis: .vertical)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .font(.footnote.monospaced())
                        .lineLimit(2...6)
                    // The system paste control: no clipboard permission prompt.
                    PasteButton(payloadType: String.self) { strings in
                        Task { @MainActor in pasted = strings.first ?? "" }
                    }
                    .buttonBorderShape(.capsule)
                    .labelStyle(.titleAndIcon)
                } footer: {
                    Text("Copy the pairing code shown under the QR code on the computer, then paste it here.")
                }
            }
            .navigationTitle("Paste Code")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { showPaste = false }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Pair") {
                        showPaste = false
                        flow.begin(qr: pasted, deviceName: deviceName, model: model)
                    }
                    .disabled(pasted.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
        }
        .presentationDetents([.medium])
    }

    private func progressStep(host: String) -> some View {
        ProgressView("Connecting to \(host)…")
            .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private func confirmStep(host: String, code: String) -> some View {
        VStack(spacing: 16) {
            Spacer()
            Text("Confirm on \(host)")
                .font(.footnote)
                .textCase(.uppercase)
                .foregroundStyle(.secondary)
            Text(code)
                .font(.system(.largeTitle, design: .monospaced, weight: .semibold))
                .accessibilityLabel(code.map { String($0) }.joined(separator: " "))
            Text("Check that the desktop shows the same code, then confirm there. If the codes differ, refuse on the desktop.")
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
            ProgressView("Waiting for confirmation…")
                .padding(.top, 8)
            Spacer()
        }
        .padding(.horizontal, 32)
    }

    private func failedStep(_ message: String) -> some View {
        ContentUnavailableView {
            Label("Pairing Failed", systemImage: "xmark.octagon")
        } description: {
            Text(message + "\n\nShow a new QR code on the desktop and try again.")
        } actions: {
            Button("Try Again") {
                pasted = ""
                flow.reset()
            }
            .buttonStyle(.borderedProminent)
        }
    }

    private func checkCamera() async {
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized:
            cameraAllowed = true
        case .notDetermined:
            cameraAllowed = await AVCaptureDevice.requestAccess(for: .video)
        default:
            cameraAllowed = false
        }
    }
}

/// A numbered step in the pairing panel.
private struct PairStep: View {
    let number: Int
    let text: Text

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            Text("\(number)")
                .font(.footnote.weight(.semibold))
                .frame(width: 22, height: 22)
                .background(Color.white.opacity(0.14), in: Circle())
                .alignmentGuide(.firstTextBaseline) { $0[VerticalAlignment.center] + 5 }
            text
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}

/// Four corner brackets around the area where the QR code should be.
private struct Viewfinder: View {
    var body: some View {
        ZStack {
            Corners()
                .stroke(.white, style: StrokeStyle(lineWidth: 4, lineCap: .round, lineJoin: .round))
        }
        .accessibilityHidden(true)
    }

    private struct Corners: Shape {
        func path(in r: CGRect) -> Path {
            let len: CGFloat = 44
            let rad: CGFloat = 18
            var p = Path()
            // Top left
            p.move(to: CGPoint(x: r.minX, y: r.minY + len))
            p.addLine(to: CGPoint(x: r.minX, y: r.minY + rad))
            p.addQuadCurve(to: CGPoint(x: r.minX + rad, y: r.minY), control: CGPoint(x: r.minX, y: r.minY))
            p.addLine(to: CGPoint(x: r.minX + len, y: r.minY))
            // Top right
            p.move(to: CGPoint(x: r.maxX - len, y: r.minY))
            p.addLine(to: CGPoint(x: r.maxX - rad, y: r.minY))
            p.addQuadCurve(to: CGPoint(x: r.maxX, y: r.minY + rad), control: CGPoint(x: r.maxX, y: r.minY))
            p.addLine(to: CGPoint(x: r.maxX, y: r.minY + len))
            // Bottom right
            p.move(to: CGPoint(x: r.maxX, y: r.maxY - len))
            p.addLine(to: CGPoint(x: r.maxX, y: r.maxY - rad))
            p.addQuadCurve(to: CGPoint(x: r.maxX - rad, y: r.maxY), control: CGPoint(x: r.maxX, y: r.maxY))
            p.addLine(to: CGPoint(x: r.maxX - len, y: r.maxY))
            // Bottom left
            p.move(to: CGPoint(x: r.minX + len, y: r.maxY))
            p.addLine(to: CGPoint(x: r.minX + rad, y: r.maxY))
            p.addQuadCurve(to: CGPoint(x: r.minX, y: r.maxY - rad), control: CGPoint(x: r.minX, y: r.maxY))
            p.addLine(to: CGPoint(x: r.minX, y: r.maxY - len))
            return p
        }
    }
}

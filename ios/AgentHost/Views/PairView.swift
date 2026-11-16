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

/// Pair screen: scan (or paste) the QR code from the desktop, then compare
/// the confirmation code.
struct PairView: View {
    @EnvironmentObject private var model: AppModel
    @StateObject private var flow = PairFlow()
    @State private var pasted = ""
    @State private var deviceName = ""
    @State private var cameraAllowed: Bool?

    var body: some View {
        NavigationStack {
            Group {
                switch flow.step {
                case .scan:
                    scanStep
                case let .pairing(host):
                    progressStep(host: host)
                case let .confirm(host, code):
                    confirmStep(host: host, code: code)
                case let .failed(message):
                    failedStep(message)
                }
            }
            .navigationTitle("Pair with your host")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                if isInProgress {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Cancel") { flow.cancel() }
                    }
                }
            }
        }
        .task {
            if deviceName.isEmpty { deviceName = UIDevice.current.name }
            await checkCamera()
        }
    }

    private var isInProgress: Bool {
        switch flow.step {
        case .pairing, .confirm: return true
        default: return false
        }
    }

    private var scanStep: some View {
        Form {
            Section {
                ZStack {
                    if cameraAllowed == true {
                        QRScannerView { code in
                            flow.begin(qr: code, deviceName: deviceName, model: model)
                        }
                    } else {
                        Color.black
                        if cameraAllowed == false {
                            ContentUnavailableView(
                                "Camera Access Is Off",
                                systemImage: "camera.fill",
                                description: Text("Allow it in Settings, or paste the code below.")
                            )
                        } else {
                            ProgressView("Starting camera…")
                        }
                    }
                }
                .frame(height: 280)
                .listRowInsets(EdgeInsets())
            } footer: {
                Text("On the computer running AgentHost, open Devices and pair a device (or run `agenthost device pair`), then scan the QR code.")
            }

            if let err = flow.scanError {
                Section {
                    Label(err, systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange)
                }
            }

            Section("This iPhone's name") {
                TextField("Device name", text: $deviceName)
                    .textInputAutocapitalization(.words)
            }

            Section("Or paste the pairing code") {
                TextField("agenthost://pair?…", text: $pasted, axis: .vertical)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .font(.footnote.monospaced())
                    .lineLimit(1...4)
                HStack {
                    // The system paste control: no clipboard permission prompt.
                    PasteButton(payloadType: String.self) { strings in
                        Task { @MainActor in pasted = strings.first ?? "" }
                    }
                    .buttonBorderShape(.capsule)
                    .labelStyle(.titleAndIcon)
                    Spacer()
                    Button("Pair") {
                        flow.begin(qr: pasted, deviceName: deviceName, model: model)
                    }
                    .disabled(pasted.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
        }
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

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
        }
        .task {
            if deviceName.isEmpty { deviceName = UIDevice.current.name }
            await checkCamera()
        }
    }

    private var scanStep: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text("On the computer running AgentHost, open Devices → Pair a device (or run `agenthost device pair`), then scan the QR code.")
                    .font(.callout)
                    .foregroundStyle(.secondary)

                ZStack {
                    if cameraAllowed == true {
                        QRScannerView { code in
                            flow.begin(qr: code, deviceName: deviceName, model: model)
                        }
                    } else {
                        Color.black
                        Text(cameraAllowed == false
                             ? "Camera access is off. Allow it in Settings, or paste the code below."
                             : "Starting camera…")
                            .foregroundStyle(.white)
                            .multilineTextAlignment(.center)
                            .padding()
                    }
                }
                .frame(height: 300)
                .clipShape(RoundedRectangle(cornerRadius: 16))

                if let err = flow.scanError {
                    Label(err, systemImage: "exclamationmark.triangle")
                        .foregroundStyle(.orange)
                        .font(.footnote)
                }

                VStack(alignment: .leading, spacing: 8) {
                    Text("This iPhone's name").font(.subheadline.bold())
                    TextField("Device name", text: $deviceName)
                        .textFieldStyle(.roundedBorder)
                        .textInputAutocapitalization(.words)
                }

                VStack(alignment: .leading, spacing: 8) {
                    Text("Or paste the pairing code").font(.subheadline.bold())
                    TextField("agenthost://pair?…", text: $pasted, axis: .vertical)
                        .textFieldStyle(.roundedBorder)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .font(.footnote.monospaced())
                        .lineLimit(1...4)
                    HStack {
                        Button {
                            if let s = UIPasteboard.general.string { pasted = s }
                        } label: {
                            Label("Paste", systemImage: "doc.on.clipboard")
                        }
                        .buttonStyle(.bordered)
                        Spacer()
                        Button("Pair") {
                            flow.begin(qr: pasted, deviceName: deviceName, model: model)
                        }
                        .buttonStyle(.borderedProminent)
                        .disabled(pasted.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    }
                }
            }
            .padding()
        }
    }

    private func progressStep(host: String) -> some View {
        VStack(spacing: 16) {
            Spacer()
            ProgressView()
            Text("Connecting to \(host)…")
                .foregroundStyle(.secondary)
            Button("Cancel", role: .cancel) { flow.cancel() }
            Spacer()
        }
        .padding()
    }

    private func confirmStep(host: String, code: String) -> some View {
        VStack(spacing: 20) {
            Spacer()
            Text("Confirm on \(host)")
                .font(.title3.bold())
            Text(code)
                .font(.system(size: 44, weight: .semibold, design: .monospaced))
                .accessibilityLabel(code.map { String($0) }.joined(separator: " "))
            Text("Check that the desktop shows the same code, then confirm there. If the codes differ, refuse on the desktop.")
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
                .padding(.horizontal)
            ProgressView("Waiting for confirmation…")
            Button("Cancel", role: .cancel) { flow.cancel() }
            Spacer()
        }
        .padding()
    }

    private func failedStep(_ message: String) -> some View {
        VStack(spacing: 20) {
            Spacer()
            Image(systemName: "xmark.octagon")
                .font(.system(size: 44))
                .foregroundStyle(.red)
            Text(message)
                .multilineTextAlignment(.center)
                .padding(.horizontal)
            Text("Show a new QR code on the desktop and try again.")
                .font(.footnote)
                .foregroundStyle(.secondary)
            Button("Try again") {
                pasted = ""
                flow.reset()
            }
            .buttonStyle(.borderedProminent)
            Spacer()
        }
        .padding()
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

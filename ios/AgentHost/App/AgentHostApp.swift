import SwiftUI

@main
struct AgentHostApp: App {
    @StateObject private var model = AppModel()

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(model)
        }
    }
}

/// Chooses the screen for the current phase, locks on background, and
/// covers everything whenever the scene is not active (so the app-switcher
/// snapshot shows nothing).
struct RootView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.scenePhase) private var scenePhase
    /// One automatic Face ID prompt per return to the foreground; after a
    /// cancel the user taps Unlock (avoids a prompt loop).
    @State private var autoUnlockTried = false

    var body: some View {
        ZStack {
            switch model.phase {
            case .locked:
                LockView()
            case .unpaired:
                PairView()
            case .ready:
                SessionsView()
            case .removed:
                RemovedView()
            }
            if scenePhase != .active {
                PrivacyCover()
                    .transition(.identity)
            }
        }
        .onChange(of: scenePhase) { _, newPhase in
            switch newPhase {
            case .background:
                model.lock()
                autoUnlockTried = false
            case .active:
                tryAutoUnlock()
            default:
                break
            }
        }
        .onAppear { tryAutoUnlock() }
    }

    private func tryAutoUnlock() {
        guard scenePhase == .active, model.phase == .locked, !autoUnlockTried else { return }
        autoUnlockTried = true
        Task { await model.unlock() }
    }
}

/// Shown instead of any content while the scene is inactive or in the background.
struct PrivacyCover: View {
    var body: some View {
        ZStack {
            Color(uiColor: .systemBackground).ignoresSafeArea()
            VStack(spacing: 12) {
                Image(systemName: "lock.shield")
                    .font(.system(size: 48))
                    .foregroundStyle(.secondary)
                Text("AgentHost")
                    .font(.headline)
                    .foregroundStyle(.secondary)
            }
        }
        .accessibilityHidden(true)
    }
}

struct LockView: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        VStack(spacing: 20) {
            Spacer()
            Image(systemName: "lock.fill")
                .font(.system(size: 52))
                .foregroundStyle(.tint)
            Text("AgentHost is locked")
                .font(.title2.bold())
            Text("Use Face ID or your passcode to continue.")
                .foregroundStyle(.secondary)
            if let err = model.lockError {
                Text(err)
                    .font(.footnote)
                    .foregroundStyle(.red)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal)
            }
            Button {
                Task { await model.unlock() }
            } label: {
                Label("Unlock", systemImage: "faceid")
                    .frame(maxWidth: 240)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            Spacer()
        }
        .padding()
    }
}

struct RemovedView: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        VStack(spacing: 20) {
            Spacer()
            Image(systemName: "iphone.slash")
                .font(.system(size: 52))
                .foregroundStyle(.red)
            Text("This device was removed")
                .font(.title2.bold())
            Text("The host no longer accepts this iPhone. Its keys and cached transcripts have been deleted.")
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
                .padding(.horizontal)
            Button("Pair again") { model.pairAgain() }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
            Spacer()
        }
        .padding()
    }
}

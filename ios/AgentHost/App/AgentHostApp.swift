import SwiftUI

@main
struct AgentHostApp: App {
    @StateObject private var model = AppModel()

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(model)
                // Dark only (also set by UIUserInterfaceStyle in Info.plist,
                // which covers the launch screen and system UI).
                .preferredColorScheme(.dark)
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
                    .font(.largeTitle)
                    .imageScale(.large)
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
        ContentUnavailableView {
            Label("AgentHost Is Locked", systemImage: "lock.fill")
        } description: {
            Text("Use Face ID or your passcode to continue.")
            if let err = model.lockError {
                Text(err).foregroundStyle(.red)
            }
        } actions: {
            Button {
                Task { await model.unlock() }
            } label: {
                Label("Unlock", systemImage: "faceid")
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
        }
    }
}

struct RemovedView: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        ContentUnavailableView {
            Label("This Device Was Removed", systemImage: "iphone.slash")
        } description: {
            Text("The host no longer accepts this iPhone. Its keys and cached transcripts have been deleted.")
        } actions: {
            Button("Pair Again") { model.pairAgain() }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
        }
    }
}

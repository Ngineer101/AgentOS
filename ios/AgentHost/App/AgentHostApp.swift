import SwiftUI

@main
struct AgentHostApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
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

/// The app is portrait; the host screen viewer unlocks landscape while it is
/// open (a desktop is wide).
enum OrientationLock {
    @MainActor static var mask: UIInterfaceOrientationMask = .portrait

    @MainActor static func set(_ mask: UIInterfaceOrientationMask) {
        self.mask = mask
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        for scene in scenes {
            for window in scene.windows {
                var vc = window.rootViewController
                while let v = vc {
                    v.setNeedsUpdateOfSupportedInterfaceOrientations()
                    vc = v.presentedViewController
                }
            }
            if mask == .portrait, scene.interfaceOrientation != .portrait {
                scene.requestGeometryUpdate(.iOS(interfaceOrientations: .portrait))
            }
        }
    }
}

final class AppDelegate: NSObject, UIApplicationDelegate {
    func application(_ application: UIApplication, supportedInterfaceOrientationsFor window: UIWindow?) -> UIInterfaceOrientationMask {
        OrientationLock.mask
    }
}

/// Chooses the screen for the current phase, locks on background (when Face ID
/// is required), and
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
                model.enterBackground()
                autoUnlockTried = false
            case .active:
                tryAutoUnlock()
                model.enterForeground()
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
                Image("Logo")
                    .resizable()
                    .scaledToFit()
                    .frame(width: 96, height: 96)
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
            Text(model.requireUnlock || model.lockError != nil
                 ? "Use \(AppModel.unlockMethodName) or your passcode to continue."
                 : "Opening…")
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

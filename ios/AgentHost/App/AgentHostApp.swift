import SwiftUI
import UserNotifications

@main
struct AgentHostApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var model = AppModel()

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(model)
                .onAppear { appDelegate.model = model }
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

final class AppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    /// Set when the first view appears. A notification tapped before then
    /// (the app was not running) waits in `pendingOpen`.
    weak var model: AppModel? {
        didSet {
            if let model, let info = pendingOpen {
                pendingOpen = nil
                model.openFromNotification(info)
            }
        }
    }
    private var pendingOpen: [AnyHashable: Any]?

    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        // Before launch finishes, so a tap that launched the app is delivered.
        UNUserNotificationCenter.current().delegate = self
        return true
    }

    func application(_ application: UIApplication, supportedInterfaceOrientationsFor window: UIWindow?) -> UIInterfaceOrientationMask {
        OrientationLock.mask
    }

    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        model?.didRegisterForRemoteNotifications(token: deviceToken)
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {
        model?.didFailToRegisterForRemoteNotifications(error)
    }

    // MARK: UNUserNotificationCenterDelegate

    /// In the foreground: quiet when that session is on screen, otherwise a
    /// banner.
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        let sid = notification.request.content.userInfo[PushUserInfo.sessionId] as? String
        DispatchQueue.main.async {
            MainActor.assumeIsolated {
                let quiet = self.model?.isShowing(sessionId: sid) ?? false
                completionHandler(quiet ? [] : [.banner, .list, .sound])
            }
        }
    }

    /// Tapped: open that session (an approval's session shows its status
    /// card, which leads to the approval).
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let info = response.notification.request.content.userInfo
        let sid = info[PushUserInfo.sessionId] as? String
        DispatchQueue.main.async {
            MainActor.assumeIsolated {
                if let sid {
                    let wanted: [AnyHashable: Any] = [PushUserInfo.sessionId: sid]
                    if let model = self.model {
                        model.openFromNotification(wanted)
                    } else {
                        self.pendingOpen = wanted
                    }
                }
                completionHandler()
            }
        }
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
    /// The cover stays up after the scene turns active until its exit
    /// animation (the logo spins away) has played. Up at launch, so the logo
    /// is on the first frame.
    @State private var cover: CoverState = .shown
    /// Bumped whenever the cover starts or cuts short an exit, so an exit
    /// that was interrupted (the scene left active mid-animation) doesn't
    /// hide the cover when its animation completes.
    @State private var coverExit = 0

    var body: some View {
        ZStack {
            switch model.phase {
            case .locked:
                LockView()
            case .unpaired:
                PairView()
            case .ready:
                SessionsView()
                    // After pairing, once connected: offer notifications.
                    .fullScreenCover(isPresented: $model.showPushOnboarding) {
                        PushOnboardingView()
                            .environmentObject(model)
                    }
            case .removed:
                RemovedView()
            }
            // Derived from scenePhase as well as `cover`, so the cover is up
            // in the same frame the scene stops being active.
            if scenePhase != .active || cover != .hidden {
                PrivacyCover(leaving: scenePhase == .active && cover == .leaving)
                    .transition(.identity)
                    .allowsHitTesting(cover != .leaving)
            }
        }
        .onChange(of: scenePhase) { _, newPhase in
            switch newPhase {
            case .background:
                model.enterBackground()
                autoUnlockTried = false
                showCover()
            case .active:
                tryAutoUnlock()
                model.enterForeground()
                dismissCover()
            default:
                showCover()
            }
        }
        .onAppear {
            tryAutoUnlock()
            // The scene can already be active on the first frame, with no
            // onChange to follow.
            if scenePhase == .active { dismissCover() }
        }
    }

    private enum CoverState { case shown, leaving, hidden }

    /// Back up at once, without animating (privacy), cutting short any exit.
    private func showCover() {
        coverExit += 1
        var t = Transaction()
        t.disablesAnimations = true
        withTransaction(t) { cover = .shown }
    }

    /// Plays the cover's exit (the logo spins and shrinks away over the app as
    /// the backdrop fades), then takes it out.
    private func dismissCover() {
        guard cover == .shown else { return }
        coverExit += 1
        let exit = coverExit
        withAnimation(PrivacyCover.exitAnimation) {
            cover = .leaving
        } completion: {
            if coverExit == exit { cover = .hidden }
        }
    }

    private func tryAutoUnlock() {
        guard scenePhase == .active, model.phase == .locked, !autoUnlockTried else { return }
        autoUnlockTried = true
        Task { await model.unlock() }
    }
}

/// Shown instead of any content while the scene is inactive or in the
/// background, and as the splash at launch. `leaving` is its exit: the logo
/// spins as it shrinks away while the backdrop fades and the app shows through
/// (with Reduce Motion the logo fades instead).
struct PrivacyCover: View {
    var leaving = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    static let exitAnimation = Animation.easeIn(duration: 0.7)

    var body: some View {
        ZStack {
            Color(uiColor: .systemBackground)
                .ignoresSafeArea()
                .opacity(leaving ? 0 : 1)
            VStack(spacing: 12) {
                Image("Logo")
                    .resizable()
                    .scaledToFit()
                    .frame(width: 96, height: 96)
                    .rotationEffect(.degrees(leaving && !reduceMotion ? 360 : 0))
                    .scaleEffect(leaving && !reduceMotion ? 0.01 : 1)
                    .opacity(leaving && reduceMotion ? 0 : 1)
                Text("AgentOS")
                    .font(.headline)
                    .foregroundStyle(.secondary)
                    .opacity(leaving ? 0 : 1)
            }
        }
        .accessibilityHidden(true)
    }
}

struct LockView: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        ContentUnavailableView {
            Label("AgentOS Is Locked", systemImage: "lock.fill")
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

import SwiftUI

// Glass controls: iOS 26 Liquid Glass where available, a material look-alike
// on iOS 17–25.

/// Colours from the design tokens (`ios/*`), all standard dark system colours.
enum Palette {
    static let cell = Color(uiColor: .secondarySystemGroupedBackground)
    static let fill = Color(uiColor: .tertiarySystemBackground)
    static let orange = Color.orange
    static let green = Color.green
    static let red = Color.red
}

extension View {
    /// Glass behind this view in `shape` (optionally tinted, optionally
    /// reacting to touches).
    @ViewBuilder
    func glassSurface<S: Shape>(_ shape: S, tint: Color? = nil, interactive: Bool = false) -> some View {
        if #available(iOS 26, *) {
            glassEffect(Self.glass(tint: tint, interactive: interactive), in: shape)
        } else {
            background {
                if let tint {
                    shape.fill(tint)
                } else {
                    shape.fill(.ultraThinMaterial)
                        .overlay(shape.fill(Color(white: 0.47, opacity: 0.16)))
                }
            }
            .overlay(shape.stroke(Color.white.opacity(tint == nil ? 0.16 : 0.0), lineWidth: 0.5))
            .shadow(color: .black.opacity(0.35), radius: 8, y: 2)
        }
    }

    @available(iOS 26, *)
    private static func glass(tint: Color?, interactive: Bool) -> Glass {
        var g = Glass.regular
        if let tint { g = g.tint(tint) }
        if interactive { g = g.interactive() }
        return g
    }

    /// Groups nearby glass shapes so they blend (iOS 26); a no-op before.
    @ViewBuilder
    func glassGroup(spacing: CGFloat = 10) -> some View {
        if #available(iOS 26, *) {
            GlassEffectContainer(spacing: spacing) { self }
        } else {
            self
        }
    }

    /// A bar along the bottom that content scrolls under: `safeAreaBar`
    /// (with the scroll edge effect) on iOS 26, `safeAreaInset` before.
    @ViewBuilder
    func bottomGlassBar<C: View>(@ViewBuilder _ content: () -> C) -> some View {
        if #available(iOS 26, *) {
            safeAreaBar(edge: .bottom, content: content)
        } else {
            safeAreaInset(edge: .bottom, content: content)
        }
    }

    /// Title and a secondary line in the navigation bar: the system subtitle
    /// on iOS 26, a two-line principal item before.
    @ViewBuilder
    func navTitle(_ title: String, subtitle: String?) -> some View {
        if #available(iOS 26, *) {
            if let subtitle {
                navigationTitle(title).navigationSubtitle(subtitle)
            } else {
                navigationTitle(title)
            }
        } else {
            navigationTitle(title)
                .toolbar {
                    ToolbarItem(placement: .principal) {
                        VStack(spacing: 0) {
                            Text(title)
                                .font(.headline)
                                .lineLimit(1)
                            if let subtitle {
                                Text(subtitle)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                    .lineLimit(1)
                            }
                        }
                    }
                }
        }
    }
}

/// A toolbar icon. iOS 26 puts toolbar buttons in glass circles itself;
/// before that the icon gets a material circle of its own.
struct ToolbarIcon: View {
    let systemName: String

    var body: some View {
        if #available(iOS 26, *) {
            Image(systemName: systemName)
        } else {
            Image(systemName: systemName)
                .font(.body.weight(.medium))
                .foregroundStyle(Color.primary)
                .frame(width: 36, height: 36)
                .glassSurface(Circle())
        }
    }
}

/// A round 44 pt glass button (attach, discard dictation).
struct GlassCircleButtonStyle: ButtonStyle {
    var size: CGFloat = 44
    var tint: Color? = nil

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.title3.weight(.medium))
            .frame(width: size, height: size)
            .contentShape(Circle())
            .glassSurface(Circle(), tint: tint, interactive: true)
            .opacity(configuration.isPressed ? 0.7 : 1)
    }
}

/// A full-width filled capsule (Approve, Deny, Paste Code), 44 pt or more.
struct CapsuleButtonStyle: ButtonStyle {
    var fill: Color
    var foreground: Color = .white
    var height: CGFloat = 44
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.body.weight(.semibold))
            .foregroundStyle(foreground)
            .frame(maxWidth: .infinity, minHeight: height)
            .background(fill, in: Capsule())
            .contentShape(Capsule())
            .opacity(configuration.isPressed ? 0.75 : (isEnabled ? 1 : 0.45))
    }
}

// MARK: - Agents and session state

/// A login's letter badge: the agent's initial on its colour.
struct AgentBadge: View {
    let agent: String
    var dimmed = false
    var size: CGFloat = 26

    var body: some View {
        let style = Self.style(agent)
        Text(style.letter)
            .font(.system(size: size * 0.5, weight: .bold, design: .rounded))
            .foregroundStyle(style.foreground)
            .frame(width: size, height: size)
            .background(style.background, in: RoundedRectangle(cornerRadius: size * 0.27, style: .continuous))
            .opacity(dimmed ? 0.45 : 1)
            .accessibilityHidden(true)
    }

    private static func style(_ agent: String) -> (letter: String, foreground: Color, background: Color) {
        switch agent {
        case "claude": return ("C", .white, Color(red: 0.85, green: 0.47, blue: 0.34))
        case "codex": return ("X", .black, .white)
        case "gemini": return ("G", .white, Color(red: 0.26, green: 0.38, blue: 0.96))
        case "opencode": return ("O", .white, Color(white: 0.32))
        case "cursor": return ("C", .black, Color(white: 0.85))
        default:
            let letter = agentDisplayName(agent).prefix(1).uppercased()
            return (letter.isEmpty ? "?" : letter, .white, Color(white: 0.32))
        }
    }
}

/// The one status vocabulary: orange = needs you, spinner = running,
/// ring = idle, check = ended, red = failed. Quick tasks that are idle show
/// their bolt.
struct SessionStatusIcon: View {
    let state: String
    var quickTask = false

    var body: some View {
        ZStack {
            Circle().fill(background)
            glyph
        }
        .frame(width: 30, height: 30)
        .accessibilityHidden(true)
    }

    @ViewBuilder
    private var glyph: some View {
        switch state {
        case SessionStateName.waitingApproval:
            Circle().fill(Palette.orange).frame(width: 10, height: 10)
        case SessionStateName.running, SessionStateName.starting, SessionStateName.created:
            Spinner()
        case SessionStateName.failed:
            Image(systemName: "exclamationmark")
                .font(.system(size: 13, weight: .bold))
                .foregroundStyle(Palette.red)
        case SessionStateName.ended:
            Image(systemName: "checkmark")
                .font(.system(size: 11, weight: .bold))
                .foregroundStyle(.secondary)
        default:
            if quickTask {
                Image(systemName: "bolt.fill")
                    .font(.system(size: 13))
                    .foregroundStyle(Palette.orange)
            } else {
                Circle()
                    .strokeBorder(Color.secondary, lineWidth: 1.8)
                    .frame(width: 10, height: 10)
            }
        }
    }

    private var background: Color {
        switch state {
        case SessionStateName.waitingApproval: return Palette.orange.opacity(0.2)
        case SessionStateName.running, SessionStateName.starting, SessionStateName.created: return Color.blue.opacity(0.2)
        case SessionStateName.failed: return Palette.red.opacity(0.2)
        case SessionStateName.ended: return Color.white.opacity(0.08)
        default: return quickTask ? Palette.orange.opacity(0.2) : Color.white.opacity(0.08)
        }
    }
}

/// A small blue arc that turns: a running session.
struct Spinner: View {
    var size: CGFloat = 16
    @State private var turning = false

    var body: some View {
        ZStack {
            Circle().stroke(Color.blue.opacity(0.35), lineWidth: 1.8)
            Circle()
                .trim(from: 0, to: 0.25)
                .stroke(Color.blue, style: StrokeStyle(lineWidth: 1.8, lineCap: .round))
                .rotationEffect(.degrees(turning ? 360 : 0))
        }
        .frame(width: size - 2, height: size - 2)
        .onAppear {
            withAnimation(.linear(duration: 1).repeatForever(autoreverses: false)) { turning = true }
        }
    }
}

/// "now", "3m", "2h", "Yesterday", "Mon", "12 Nov".
func shortAge(_ ms: Int64, now: Date = Date()) -> String {
    let date = dateFromMillis(ms)
    let seconds = now.timeIntervalSince(date)
    if seconds < 60 { return "now" }
    if seconds < 3600 { return "\(Int(seconds / 60))m" }
    let cal = Calendar.current
    if cal.isDateInToday(date) || seconds < 6 * 3600 { return "\(Int(seconds / 3600))h" }
    if cal.isDateInYesterday(date) { return "Yesterday" }
    if seconds < 6 * 86400 { return date.formatted(.dateTime.weekday(.abbreviated)) }
    return date.formatted(.dateTime.day().month(.abbreviated))
}

/// "~/Code/relay" for a path in the home folder on the host (best effort:
/// the host's home is not known, so /Users/<name> and /home/<name> are
/// shortened).
func abbreviatedPath(_ path: String) -> String {
    let parts = path.split(separator: "/", omittingEmptySubsequences: true)
    if parts.count >= 2, parts[0] == "Users" || parts[0] == "home" {
        let rest = parts.dropFirst(2).joined(separator: "/")
        return rest.isEmpty ? "~" : "~/" + rest
    }
    return path
}

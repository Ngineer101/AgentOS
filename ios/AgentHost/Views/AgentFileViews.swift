import AVFoundation
import AVKit
import QuickLook
import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// A file the agent sent (`agent.file`), in its reply: a recording as its
/// poster with a play button and its length, an image as a picture, any
/// other file as a row with its name and size. The agent's caption under it.
struct AgentFileView: View {
    let sessionId: String
    let file: AgentFileInfo
    var sentAt: Date?

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if file.isVideo {
                AgentRecordingThumbnail(sessionId: sessionId, file: file)
            } else if file.isImage {
                AgentImageThumbnail(sessionId: sessionId, file: file, sentAt: sentAt)
            } else {
                AgentFileRow(sessionId: sessionId, file: file)
            }
            if let caption = file.caption {
                Text(caption)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// Pictures and posters fit in 280 × 320 pt at their own aspect ratio.
private enum AgentMedia {
    static let maxSize = CGSize(width: 280, height: 320)
    /// 16:10, the shape of a browser recording, when the size is unknown.
    static let fallback = CGSize(width: 280, height: 175)
    static let shape = RoundedRectangle(cornerRadius: 18, style: .continuous)

    static func fitted(_ size: CGSize?) -> CGSize {
        guard let size, size.width > 0, size.height > 0 else { return fallback }
        let scale = min(maxSize.width / size.width, maxSize.height / size.height, 1)
        return CGSize(width: max(44, (size.width * scale).rounded()),
                      height: max(44, (size.height * scale).rounded()))
    }
}

extension View {
    /// "Can't Open File" (or `title`) while `message` is set.
    fileprivate func failureAlert(_ title: String, _ message: Binding<String?>) -> some View {
        alert(title, isPresented: Binding(
            get: { message.wrappedValue != nil },
            set: { if !$0 { message.wrappedValue = nil } }
        )) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(message.wrappedValue ?? "")
        }
    }
}

// MARK: - Recording

/// A video to play: its local file.
private struct PlayableVideo: Identifiable {
    let url: URL
    var id: URL { url }
}

/// A recording's poster with a play button and its length. Tap: download
/// (a progress bar over the poster), then play full screen.
struct AgentRecordingThumbnail: View {
    @EnvironmentObject private var model: AppModel
    let sessionId: String
    let file: AgentFileInfo

    @State private var poster: UIImage?
    /// 0...1 while downloading.
    @State private var progress: Double?
    @State private var failure: String?
    @State private var playing: PlayableVideo?

    var body: some View {
        let size = AgentMedia.fitted(file.pixelSize ?? poster?.size)
        Button(action: play) {
            ZStack {
                if let poster {
                    Image(uiImage: poster)
                        .resizable()
                        .scaledToFill()
                } else {
                    Color(uiColor: .systemGray5)
                }
            }
            .frame(width: size.width, height: size.height)
            .clipShape(AgentMedia.shape)
            .overlay(AgentMedia.shape.strokeBorder(Color.white.opacity(0.15), lineWidth: 0.5))
            .overlay {
                Group {
                    if progress != nil {
                        ProgressView()
                    } else {
                        Image(systemName: "play.fill")
                            .font(.title2)
                            .foregroundStyle(.white)
                    }
                }
                .frame(width: 56, height: 56)
                .background(.ultraThinMaterial, in: Circle())
            }
            .overlay(alignment: .bottomTrailing) {
                if progress == nil, let ms = file.durationMs {
                    Text(formatClock(ms: ms))
                        .font(.caption.weight(.semibold))
                        .monospacedDigit()
                        .foregroundStyle(.white)
                        .shadow(color: .black.opacity(0.6), radius: 3)
                        .padding(.horizontal, 10)
                        .padding(.bottom, 8)
                }
            }
            .overlay(alignment: .bottom) {
                if let progress {
                    ProgressView(value: progress)
                        .padding(.horizontal, 14)
                        .padding(.bottom, 12)
                }
            }
            .contentShape(AgentMedia.shape)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(accessibilityLabel)
        .accessibilityHint(progress == nil ? "Plays the recording" : "Downloading")
        .task(id: file.poster?.attachmentId) { await loadPoster() }
        .fullScreenCover(item: $playing) { video in
            AgentVideoPlayer(url: video.url, file: file)
        }
        .failureAlert("Can't Play Recording", $failure)
    }

    private var accessibilityLabel: String {
        var label = file.kind == .recording ? "Recording" : file.attachment.name
        if let ms = file.durationMs { label += ", \(formatDuration(ms: ms))" }
        return label
    }

    private func loadPoster() async {
        guard poster == nil, let p = file.poster else { return }
        if let img = SentImages.shared.decoded(p.attachmentId) {
            poster = img
            return
        }
        poster = try? await model.agentImage(sessionId: sessionId, attachment: p)
    }

    private func play() {
        guard progress == nil else { return }
        if let url = AgentFiles.cached(sessionId: sessionId, attachment: file.attachment) {
            playing = PlayableVideo(url: url)
            return
        }
        progress = 0
        Task {
            do {
                let url = try await model.agentFile(sessionId: sessionId, attachment: file.attachment) { p in
                    progress = p
                }
                progress = nil
                playing = PlayableVideo(url: url)
            } catch {
                progress = nil
                failure = userMessage(for: error)
            }
        }
    }
}

/// A downloaded recording full screen with AVKit's player and its own
/// controls; close and Share (Save Video, AirDrop…) at the top, the
/// caption under it.
struct AgentVideoPlayer: View {
    @Environment(\.dismiss) private var dismiss
    let url: URL
    let file: AgentFileInfo
    @State private var player: AVPlayer

    init(url: URL, file: AgentFileInfo) {
        self.url = url
        self.file = file
        _player = State(initialValue: AVPlayer(url: url))
    }

    var body: some View {
        NavigationStack {
            VideoPlayer(player: player)
                .safeAreaInset(edge: .bottom) {
                    if let caption = file.caption {
                        Text(caption)
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                            .multilineTextAlignment(.center)
                            .frame(maxWidth: .infinity)
                            .padding(.horizontal, 16)
                            .padding(.vertical, 12)
                    }
                }
                .background(Color.black)
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button {
                            dismiss()
                        } label: {
                            ToolbarIcon(systemName: "xmark")
                        }
                        .accessibilityLabel("Close")
                    }
                    ToolbarItem(placement: .principal) {
                        VStack(spacing: 0) {
                            Text(file.attachment.name).font(.headline).lineLimit(1).truncationMode(.middle)
                            Text(subtitle)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                    ToolbarItem(placement: .primaryAction) {
                        ShareLink(item: url) {
                            ToolbarIcon(systemName: "square.and.arrow.up")
                        }
                        .accessibilityLabel("Share")
                    }
                }
        }
        .onAppear {
            // Sound even with the ring switch on silent, as in Photos (and
            // after dictation left the session recording).
            try? AVAudioSession.sharedInstance().setCategory(.playback, mode: .moviePlayback)
            player.play()
        }
        .onDisappear {
            player.pause()
        }
    }

    /// "0:42 · 3.1 MB".
    private var subtitle: String {
        let size = formatBytes(file.attachment.size)
        guard let ms = file.durationMs else { return size }
        return "\(formatClock(ms: ms)) · \(size)"
    }
}

// MARK: - Image

/// An image the agent sent, like a sent image: tap to see it full screen
/// (zoom, Share). A file row if it can't be shown.
struct AgentImageThumbnail: View {
    @EnvironmentObject private var model: AppModel
    let sessionId: String
    let file: AgentFileInfo
    var sentAt: Date?

    @State private var image: UIImage?
    @State private var failure: String?
    @State private var showViewer = false

    var body: some View {
        Group {
            if let image {
                let size = AgentMedia.fitted(image.size)
                Button {
                    showViewer = true
                } label: {
                    Image(uiImage: image)
                        .resizable()
                        .scaledToFill()
                        .frame(width: size.width, height: size.height)
                        .clipShape(AgentMedia.shape)
                        .overlay(AgentMedia.shape.strokeBorder(Color.white.opacity(0.15), lineWidth: 0.5))
                        .contentShape(AgentMedia.shape)
                }
                .buttonStyle(.plain)
                .accessibilityLabel(file.caption ?? file.attachment.name)
                .accessibilityHint("Shows the image full screen")
                .fullScreenCover(isPresented: $showViewer) {
                    SentImageViewer(image: image, name: file.attachment.name, sentAt: sentAt)
                }
            } else if failure != nil {
                // QuickLook may still open it.
                AgentFileRow(sessionId: sessionId, file: file)
            } else {
                let size = AgentMedia.fitted(file.pixelSize)
                AgentMedia.shape
                    .fill(Color(uiColor: .systemGray5))
                    .frame(width: size.width, height: size.height)
                    .overlay(ProgressView())
                    .accessibilityLabel("\(file.attachment.name), loading")
            }
        }
        .task(id: file.attachment.attachmentId) { await load() }
        .onChange(of: model.link.isOnline) { _, online in
            // Couldn't load while offline: try again once connected.
            if online, image == nil, failure != nil {
                failure = nil
                Task { await load() }
            }
        }
    }

    private func load() async {
        if let img = SentImages.shared.decoded(file.attachment.attachmentId) {
            image = img
            return
        }
        do {
            image = try await model.agentImage(sessionId: sessionId, attachment: file.attachment)
        } catch {
            failure = userMessage(for: error)
        }
    }
}

// MARK: - Other files

/// Any other file: its icon, name and size. Tap: download (a progress bar
/// in place of the size), then open it in QuickLook. Share from its menu
/// once downloaded (QuickLook has Share too).
struct AgentFileRow: View {
    @EnvironmentObject private var model: AppModel
    let sessionId: String
    let file: AgentFileInfo

    /// 0...1 while downloading.
    @State private var progress: Double?
    @State private var failure: String?
    @State private var preview: URL?
    @State private var local: URL?

    var body: some View {
        Button(action: open) {
            HStack(spacing: 12) {
                Image(systemName: icon)
                    .font(.title2)
                    .foregroundStyle(.tint)
                    .frame(width: 28)
                VStack(alignment: .leading, spacing: 2) {
                    Text(file.attachment.name)
                        .foregroundStyle(.primary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    if let progress {
                        ProgressView(value: progress)
                            .padding(.vertical, 6)
                    } else {
                        Text(formatBytes(file.attachment.size))
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                }
                Spacer(minLength: 0)
                Image(systemName: "chevron.right")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(.tertiary)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 11)
            .frame(maxWidth: AgentMedia.maxSize.width, alignment: .leading)
            .background(Color(uiColor: .secondarySystemGroupedBackground),
                        in: RoundedRectangle(cornerRadius: 12, style: .continuous))
            .contentShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .combine)
        .accessibilityHint(progress == nil ? "Opens the file" : "Downloading")
        .contextMenu {
            Button(action: open) {
                Label("Open", systemImage: "eye")
            }
            if let local {
                ShareLink(item: local) {
                    Label("Share", systemImage: "square.and.arrow.up")
                }
            }
        }
        .task(id: file.attachment.attachmentId) {
            local = AgentFiles.cached(sessionId: sessionId, attachment: file.attachment)
        }
        .quickLookPreview($preview)
        .failureAlert("Can't Open File", $failure)
    }

    /// An SF Symbol for the file's type.
    private var icon: String {
        guard let type = UTType(mimeType: file.attachment.mime)
            ?? UTType(filenameExtension: (file.attachment.name as NSString).pathExtension) else { return "doc" }
        if type.conforms(to: .pdf) { return "doc.richtext" }
        if type.conforms(to: .archive) { return "doc.zipper" }
        if type.conforms(to: .image) { return "photo" }
        if type.conforms(to: .movie) { return "film" }
        if type.conforms(to: .audio) { return "waveform" }
        if type.conforms(to: .text) { return "doc.text" }
        return "doc"
    }

    private func open() {
        guard progress == nil else { return }
        if let url = AgentFiles.cached(sessionId: sessionId, attachment: file.attachment) {
            local = url
            preview = url
            return
        }
        progress = 0
        Task {
            do {
                let url = try await model.agentFile(sessionId: sessionId, attachment: file.attachment) { p in
                    progress = p
                }
                progress = nil
                local = url
                preview = url
            } catch {
                progress = nil
                failure = userMessage(for: error)
            }
        }
    }
}

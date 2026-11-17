import AVFoundation
import PhotosUI
import SwiftUI
import UIKit
import UniformTypeIdentifiers

// MARK: - Attachments being prepared in the composer

/// One chip above the prompt field: a file on its way to the host.
struct ComposerAttachment: Identifiable, Equatable {
    enum Status: Equatable {
        case uploading
        case ready(AttachmentInfo)
        case failed(String)
    }

    let id = UUID()
    var name: String
    var state: Status
}

struct ComposerError: LocalizedError {
    var message: String
    var errorDescription: String? { message }
}

/// The composer's attachments: each upload starts as soon as the file is
/// picked (copied to a temporary file first, deleted after the upload).
@MainActor
final class ComposerModel: ObservableObject {
    @Published private(set) var attachments: [ComposerAttachment] = []
    private var tasks: [UUID: Task<Void, Never>] = [:]

    var isUploading: Bool { attachments.contains { $0.state == .uploading } }

    /// Uploaded and ready to send with the prompt.
    var ready: [AttachmentInfo] {
        attachments.compactMap { a in
            if case let .ready(info) = a.state { return info }
            return nil
        }
    }

    var remainingSlots: Int { max(0, RPC.maxAttachments - attachments.count) }

    /// Files from the Files app (security-scoped URLs). Returns how many
    /// were left out because of the attachment limit.
    @discardableResult
    func addFiles(_ urls: [URL], model: AppModel) -> Int {
        let taken = Array(urls.prefix(remainingSlots))
        for url in taken {
            let name = url.lastPathComponent
            let mime = UTType(filenameExtension: url.pathExtension)?.preferredMIMEType
            start(name: name, model: model) {
                try await Task.detached(priority: .userInitiated) {
                    try ComposerFiles.copyToTemporaryFile(url, name: name)
                }.value
            } upload: { file, model in
                try await model.uploadFile(at: file, name: name, mime: mime)
            }
        }
        return urls.count - taken.count
    }

    /// Photos from the photo library, sent as JPEG (PNG and GIF unchanged).
    @discardableResult
    func addPhotos(_ items: [PhotosPickerItem], model: AppModel) -> Int {
        let taken = Array(items.prefix(remainingSlots))
        let stamp = ComposerFiles.photoStamp()
        for (i, item) in taken.enumerated() {
            let base = taken.count > 1 ? "Photo \(stamp) (\(i + 1))" : "Photo \(stamp)"
            let types = item.supportedContentTypes
            let ext = ComposerFiles.photoExtension(for: types)
            let name = "\(base).\(ext)"
            start(name: name, model: model) {
                guard let data = try await item.loadTransferable(type: Data.self) else {
                    throw ComposerError(message: "The photo could not be read.")
                }
                return try await Task.detached(priority: .userInitiated) {
                    try ComposerFiles.writePhoto(data, types: types, name: name)
                }.value
            } upload: { file, model in
                try await model.uploadFile(at: file, name: name, mime: ComposerFiles.photoMime(ext))
            }
        }
        return items.count - taken.count
    }

    func remove(_ id: UUID) {
        tasks.removeValue(forKey: id)?.cancel()
        attachments.removeAll { $0.id == id }
    }

    /// After sending: the uploaded files now belong to the prompt.
    func clear() {
        for t in tasks.values { t.cancel() }
        tasks = [:]
        attachments = []
    }

    /// Prepare a temporary file, upload it, delete it; the chip follows along.
    private func start(
        name: String,
        model: AppModel,
        prepare: @escaping () async throws -> URL,
        upload: @escaping (URL, AppModel) async throws -> AttachmentInfo
    ) {
        let chip = ComposerAttachment(name: name, state: .uploading)
        attachments.append(chip)
        let id = chip.id
        tasks[id] = Task { [weak self, weak model] in
            var file: URL?
            do {
                let f = try await prepare()
                file = f
                guard let model else { throw AgentHostError.NotConnected }
                let info = try await upload(f, model)
                ComposerFiles.removeTemporaryFile(f)
                file = nil
                self?.finish(id, .ready(info))
            } catch {
                if let f = file { ComposerFiles.removeTemporaryFile(f) }
                if !Task.isCancelled {
                    self?.finish(id, .failed(userMessage(for: error)))
                }
            }
        }
    }

    private func finish(_ id: UUID, _ state: ComposerAttachment.Status) {
        tasks[id] = nil
        guard let i = attachments.firstIndex(where: { $0.id == id }) else { return }
        attachments[i].state = state
    }
}

/// Temporary copies for upload. Not actor-bound: runs off the main thread.
enum ComposerFiles {
    /// A fresh directory per file keeps the original name without clashes.
    private static func temporaryURL(named name: String) throws -> URL {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("upload-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let safe = name.replacingOccurrences(of: "/", with: "-")
        return dir.appendingPathComponent(safe.isEmpty ? "file" : safe)
    }

    static func removeTemporaryFile(_ url: URL) {
        try? FileManager.default.removeItem(at: url.deletingLastPathComponent())
    }

    private static func tooLarge() -> ComposerError {
        ComposerError(message: "Larger than \(formatBytes(RPC.maxUploadBytes)).")
    }

    /// Copy a file picked in the Files app (security scoped, maybe still in
    /// iCloud) to a temporary file.
    static func copyToTemporaryFile(_ source: URL, name: String) throws -> URL {
        let scoped = source.startAccessingSecurityScopedResource()
        defer { if scoped { source.stopAccessingSecurityScopedResource() } }
        let dest = try temporaryURL(named: name)
        var coordinationError: NSError?
        var copyError: Error?
        // Coordinated read: downloads the file first if it is only in iCloud.
        NSFileCoordinator().coordinate(readingItemAt: source, options: [], error: &coordinationError) { url in
            do {
                let size = try url.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0
                if Int64(size) > RPC.maxUploadBytes { throw tooLarge() }
                try FileManager.default.copyItem(at: url, to: dest)
            } catch {
                copyError = error
            }
        }
        if let e = copyError ?? coordinationError {
            removeTemporaryFile(dest)
            throw e
        }
        return dest
    }

    static func photoStamp(_ date: Date = Date()) -> String {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd HH.mm"
        return f.string(from: date)
    }

    static func photoExtension(for types: [UTType]) -> String {
        guard let t = types.first else { return "jpg" }
        if t.conforms(to: .png) { return "png" }
        if t.conforms(to: .gif) { return "gif" }
        return "jpg"
    }

    static func photoMime(_ ext: String) -> String {
        switch ext {
        case "png": return "image/png"
        case "gif": return "image/gif"
        default: return "image/jpeg"
        }
    }

    /// JPEG, PNG and GIF as they are; anything else (HEIC, RAW…) re-encoded
    /// as JPEG.
    static func writePhoto(_ data: Data, types: [UTType], name: String) throws -> URL {
        var bytes = data
        let keep = types.first.map { $0.conforms(to: .jpeg) || $0.conforms(to: .png) || $0.conforms(to: .gif) } ?? false
        if !keep {
            guard let image = UIImage(data: data), let jpeg = image.jpegData(compressionQuality: 0.85) else {
                throw ComposerError(message: "The photo could not be converted to JPEG.")
            }
            bytes = jpeg
        }
        if Int64(bytes.count) > RPC.maxUploadBytes { throw tooLarge() }
        let dest = try temporaryURL(named: name)
        do {
            try bytes.write(to: dest)
        } catch {
            removeTemporaryFile(dest)
            throw error
        }
        return dest
    }
}

// MARK: - Voice recording

/// Records 16 kHz mono 16-bit WAV for the host's speech model, with a level
/// meter. Stops by itself after five minutes or when the recording is
/// interrupted (`onAutoStop`).
@MainActor
final class VoiceRecorder: ObservableObject {
    nonisolated static let maxDuration: TimeInterval = 5 * 60
    nonisolated static let meterBars = 28

    @Published private(set) var isRecording = false
    @Published private(set) var elapsed: TimeInterval = 0
    /// Recent levels, 0…1, oldest first.
    @Published private(set) var levels: [Float] = Array(repeating: 0, count: VoiceRecorder.meterBars)

    /// Called when the time limit is reached or the system stopped the
    /// recording (a phone call, for example). The view then stops normally.
    var onAutoStop: (@MainActor () -> Void)?

    private var recorder: AVAudioRecorder?
    private var fileURL: URL?
    private var meterTask: Task<Void, Never>?

    func start() throws {
        guard !isRecording else { return }
        let session = AVAudioSession.sharedInstance()
        try session.setCategory(.record, mode: .default, options: [])
        try session.setActive(true)
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("voice-\(UUID().uuidString).wav")
        let settings: [String: Any] = [
            AVFormatIDKey: Int(kAudioFormatLinearPCM),
            AVSampleRateKey: 16_000.0,
            AVNumberOfChannelsKey: 1,
            AVLinearPCMBitDepthKey: 16,
            AVLinearPCMIsFloatKey: false,
            AVLinearPCMIsBigEndianKey: false,
        ]
        do {
            let r = try AVAudioRecorder(url: url, settings: settings)
            r.isMeteringEnabled = true
            guard r.prepareToRecord(), r.record() else {
                throw ComposerError(message: "Recording could not start.")
            }
            recorder = r
        } catch {
            try? FileManager.default.removeItem(at: url)
            deactivateSession()
            throw error
        }
        fileURL = url
        elapsed = 0
        levels = Array(repeating: 0, count: Self.meterBars)
        isRecording = true
        meterTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                self.tick()
                try? await Task.sleep(nanoseconds: 50_000_000)
            }
        }
    }

    /// Stop and hand over the WAV file (the caller deletes it).
    func stop() -> URL? {
        guard isRecording else { return nil }
        recorder?.stop()
        let url = fileURL
        reset()
        return url
    }

    /// Stop and throw the recording away.
    func cancel() {
        guard isRecording else { return }
        recorder?.stop()
        if let url = fileURL { try? FileManager.default.removeItem(at: url) }
        reset()
    }

    private func reset() {
        meterTask?.cancel()
        meterTask = nil
        recorder = nil
        fileURL = nil
        isRecording = false
        deactivateSession()
    }

    private func deactivateSession() {
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }

    private func tick() {
        guard isRecording, let r = recorder else { return }
        guard r.isRecording else {
            // Interrupted by the system: keep what was recorded.
            onAutoStop?()
            return
        }
        r.updateMeters()
        let db = r.averagePower(forChannel: 0)
        // -50 dB (quiet room) … 0 dB (loud) mapped to 0…1.
        let level = max(0, min(1, (db + 50) / 50))
        levels.removeFirst()
        levels.append(level)
        elapsed = r.currentTime
        if elapsed >= Self.maxDuration {
            onAutoStop?()
        }
    }
}

// MARK: - Views

/// The chips above the prompt field.
struct AttachmentChipsRow: View {
    let attachments: [ComposerAttachment]
    var onRemove: @MainActor (UUID) -> Void

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(attachments) { a in
                    AttachmentChip(attachment: a) { onRemove(a.id) }
                }
            }
        }
    }
}

struct AttachmentChip: View {
    let attachment: ComposerAttachment
    var onRemove: @MainActor () -> Void

    var body: some View {
        HStack(spacing: 8) {
            Group {
                switch attachment.state {
                case .uploading:
                    ProgressView().controlSize(.small)
                case let .ready(info):
                    Image(systemName: info.isImage ? "photo" : "doc")
                        .foregroundStyle(.secondary)
                case .failed:
                    Image(systemName: "exclamationmark.circle.fill")
                        .foregroundStyle(.red)
                }
            }
            .frame(width: 20)
            VStack(alignment: .leading, spacing: 1) {
                Text(attachment.name)
                    .font(.footnote)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text(subtitle)
                    .font(.caption2)
                    .foregroundStyle(isFailed ? Color.red : Color.secondary)
                    .lineLimit(1)
            }
            .frame(maxWidth: 160, alignment: .leading)
            Button {
                onRemove()
            } label: {
                Image(systemName: "xmark.circle.fill")
                    .symbolRenderingMode(.hierarchical)
                    .foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Remove \(attachment.name)")
        }
        .padding(.leading, 10)
        .padding(.trailing, 8)
        .padding(.vertical, 6)
        .background(Color(uiColor: .secondarySystemBackground),
                    in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(isFailed ? Color.red.opacity(0.6) : Color(uiColor: .separator), lineWidth: 0.5)
        )
    }

    private var isFailed: Bool {
        if case .failed = attachment.state { return true }
        return false
    }

    private var subtitle: String {
        switch attachment.state {
        case .uploading: return "Uploading…"
        case let .ready(info): return formatBytes(info.size)
        case let .failed(message): return message
        }
    }
}

/// Replaces the prompt row while recording or transcribing.
struct RecordingStrip: View {
    @ObservedObject var recorder: VoiceRecorder
    let transcribing: Bool
    var onCancel: @MainActor () -> Void
    var onStop: @MainActor () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Button(role: .destructive) {
                onCancel()
            } label: {
                Image(systemName: "trash")
                    .font(.title3)
            }
            .disabled(transcribing)
            .accessibilityLabel("Discard recording")

            HStack(spacing: 8) {
                if transcribing {
                    ProgressView().controlSize(.small)
                    Text("Transcribing…")
                        .foregroundStyle(.secondary)
                    Spacer(minLength: 0)
                } else {
                    Circle()
                        .fill(Color.red)
                        .frame(width: 8, height: 8)
                    Text(timeText)
                        .font(.body.monospacedDigit())
                    LevelMeter(levels: recorder.levels)
                }
            }
            .padding(.horizontal, 14)
            .frame(minHeight: 36)
            .background(Color(uiColor: .secondarySystemBackground),
                        in: RoundedRectangle(cornerRadius: 18, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 18, style: .continuous)
                    .strokeBorder(Color(uiColor: .separator), lineWidth: 0.5)
            )
            .accessibilityElement(children: .combine)
            .accessibilityLabel(transcribing ? "Transcribing" : "Recording, \(timeText)")

            Button {
                onStop()
            } label: {
                Image(systemName: "checkmark.circle.fill").font(.title)
            }
            .disabled(transcribing)
            .accessibilityLabel("Stop recording and transcribe")
        }
    }

    private var timeText: String {
        let s = Int(recorder.elapsed)
        return String(format: "%d:%02d", s / 60, s % 60)
    }
}

struct LevelMeter: View {
    let levels: [Float]

    var body: some View {
        HStack(alignment: .center, spacing: 2) {
            ForEach(Array(levels.enumerated()), id: \.offset) { _, level in
                Capsule()
                    .fill(Color.secondary)
                    .frame(width: 3, height: 3 + CGFloat(level) * 17)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: 20, alignment: .trailing)
        .clipped()
        .animation(.linear(duration: 0.05), value: levels)
        .accessibilityHidden(true)
    }
}

/// Attachments listed under a prompt bubble (sent or pending).
struct PromptAttachmentsView: View {
    let attachments: [AttachmentInfo]

    var body: some View {
        VStack(alignment: .trailing, spacing: 4) {
            ForEach(attachments) { a in
                HStack(spacing: 4) {
                    Image(systemName: "paperclip")
                    Text(a.name)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Text(formatBytes(a.size))
                        .foregroundStyle(.secondary)
                }
                .font(.caption)
                .accessibilityElement(children: .combine)
            }
        }
    }
}

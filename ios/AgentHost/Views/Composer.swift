import AVFoundation
import PhotosUI
import Speech
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
    /// A thumbnail, for pasted and dropped images.
    var preview: UIImage? = nil
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
            start(name: name, model: model) { _ in
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
            start(name: name, model: model) { _ in
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

    /// Pasted or dropped items (images and files). Returns how many were
    /// left out because of the attachment limit.
    @discardableResult
    func addItemProviders(_ providers: [NSItemProvider], model: AppModel) -> Int {
        let taken = Array(providers.prefix(remainingSlots))
        let stamp = ComposerFiles.photoStamp()
        for (i, provider) in taken.enumerated() {
            let types = provider.registeredContentTypes
            if let imageType = types.first(where: { $0.conforms(to: .image) }) {
                let ext = ComposerFiles.photoExtension(for: [imageType])
                let base = provider.suggestedName.flatMap { $0.isEmpty ? nil : ($0 as NSString).deletingPathExtension }
                    ?? (taken.count > 1 ? "Pasted Image \(stamp) (\(i + 1))" : "Pasted Image \(stamp)")
                let name = "\(base).\(ext)"
                start(name: name, model: model) { [weak self] id in
                    let data = try await ComposerFiles.loadData(provider, type: imageType)
                    let file = try await Task.detached(priority: .userInitiated) {
                        try ComposerFiles.writePhoto(data, types: [imageType], name: name)
                    }.value
                    let thumb = await UIImage(data: data)?.byPreparingThumbnail(ofSize: CGSize(width: 96, height: 96))
                    self?.setPreview(id, thumb)
                    return file
                } upload: { file, model in
                    try await model.uploadFile(at: file, name: name, mime: ComposerFiles.photoMime(ext))
                }
            } else {
                let type = types.first(where: { $0.conforms(to: .data) }) ?? types.first ?? .data
                let ext = type.preferredFilenameExtension
                var name = provider.suggestedName ?? "Pasted File \(stamp)"
                if let ext, (name as NSString).pathExtension.isEmpty { name += ".\(ext)" }
                let fileName = name
                start(name: fileName, model: model) { _ in
                    try await ComposerFiles.copyFile(provider, type: type, name: fileName)
                } upload: { file, model in
                    try await model.uploadFile(at: file, name: fileName, mime: type.preferredMIMEType)
                }
            }
        }
        return providers.count - taken.count
    }

    #if DEBUG
    /// Demo: an uploaded image chip, as after pasting a screenshot.
    func showDemoImage(_ image: UIImage, name: String) {
        let size = Int64(image.pngData()?.count ?? 0)
        let info = AttachmentInfo(attachmentId: "demo-paste", name: name, mime: "image/png", size: size)
        attachments.append(ComposerAttachment(
            name: name, state: .ready(info),
            preview: image.preparingThumbnail(of: CGSize(width: 96, height: 96))))
    }
    #endif

    private func setPreview(_ id: UUID, _ image: UIImage?) {
        guard let image, let i = attachments.firstIndex(where: { $0.id == id }) else { return }
        attachments[i].preview = image
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
        prepare: @escaping (UUID) async throws -> URL,
        upload: @escaping (URL, AppModel) async throws -> AttachmentInfo
    ) {
        let chip = ComposerAttachment(name: name, state: .uploading)
        attachments.append(chip)
        let id = chip.id
        tasks[id] = Task { [weak self, weak model] in
            var file: URL?
            do {
                let f = try await prepare(id)
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

    /// The bytes of a pasted or dropped item.
    static func loadData(_ provider: NSItemProvider, type: UTType) async throws -> Data {
        try await withCheckedThrowingContinuation { c in
            _ = provider.loadDataRepresentation(for: type) { data, error in
                if let data { c.resume(returning: data) } else {
                    c.resume(throwing: error ?? ComposerError(message: "The item could not be read."))
                }
            }
        }
    }

    /// A pasted or dropped file, copied to a temporary file (the provider's
    /// own copy is deleted when its callback returns).
    static func copyFile(_ provider: NSItemProvider, type: UTType, name: String) async throws -> URL {
        try await withCheckedThrowingContinuation { c in
            _ = provider.loadFileRepresentation(for: type, openInPlace: false) { url, _, error in
                guard let url else {
                    c.resume(throwing: error ?? ComposerError(message: "The file could not be read."))
                    return
                }
                do {
                    let size = try url.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0
                    if Int64(size) > RPC.maxUploadBytes { throw tooLarge() }
                    let dest = try temporaryURL(named: name)
                    try FileManager.default.copyItem(at: url, to: dest)
                    c.resume(returning: dest)
                } catch {
                    c.resume(throwing: error)
                }
            }
        }
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

// MARK: - Voice input

/// The audio thread's latest input level, read by the main thread's meter.
private final class LevelBox: @unchecked Sendable {
    private let lock = NSLock()
    private var value: Float = 0
    func set(_ v: Float) { lock.lock(); value = v; lock.unlock() }
    func get() -> Float { lock.lock(); defer { lock.unlock() }; return value }
}

/// iOS's own dictation (the Speech framework), with a level meter and the
/// text so far while you talk. Recognition stays on the phone where the
/// device supports it; the host's speech model is not used here. Stops by
/// itself after five minutes or when the audio is interrupted (`onAutoStop`).
@MainActor
final class SpeechDictation: ObservableObject {
    nonisolated static let maxDuration: TimeInterval = 5 * 60
    nonisolated static let meterBars = 28

    enum Access {
        case granted
        /// Why not, for an alert that offers Settings.
        case denied(String)
        case unavailable(String)
    }

    @Published private(set) var isListening = false
    /// Waiting for the final text after Stop.
    @Published private(set) var isFinishing = false
    @Published private(set) var elapsed: TimeInterval = 0
    /// Recent levels, 0…1, oldest first.
    @Published private(set) var levels: [Float] = Array(repeating: 0, count: SpeechDictation.meterBars)
    /// The recognized text so far.
    @Published private(set) var text = ""

    /// Called when the time limit is reached or the system stopped the audio
    /// (a phone call, for example). The view then stops normally.
    var onAutoStop: (@MainActor () -> Void)?

    private let engine = AVAudioEngine()
    private let level = LevelBox()
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var task: SFSpeechRecognitionTask?
    private var startedAt = Date()
    private var meterTask: Task<Void, Never>?
    private var finished: CheckedContinuation<Void, Never>?
    /// Recognition delivered its final result or ended with an error.
    private var recognitionDone = false
    /// Counts dictations, so a timeout left from an earlier one is ignored.
    private var generation = 0

    /// Ask for speech recognition and microphone access (each once).
    static func requestAccess() async -> Access {
        var speech = SFSpeechRecognizer.authorizationStatus()
        if speech == .notDetermined {
            speech = await withCheckedContinuation { c in
                SFSpeechRecognizer.requestAuthorization { c.resume(returning: $0) }
            }
        }
        switch speech {
        case .authorized: break
        case .restricted:
            return .unavailable("Speech recognition is restricted on this iPhone.")
        default:
            return .denied("To use voice input, allow Speech Recognition for AgentHost in Settings → AgentHost.")
        }
        switch AVAudioApplication.shared.recordPermission {
        case .granted:
            return .granted
        case .undetermined:
            if await AVAudioApplication.requestRecordPermission() { return .granted }
            fallthrough
        default:
            return .denied("To use voice input, allow AgentHost to use the microphone in Settings → AgentHost.")
        }
    }

    func start() throws {
        guard !isListening, !isFinishing else { return }
        guard let recognizer = SFSpeechRecognizer(), recognizer.isAvailable else {
            throw ComposerError(message: "Dictation is not available right now.")
        }
        let session = AVAudioSession.sharedInstance()
        try session.setCategory(.record, mode: .measurement, options: .duckOthers)
        try session.setActive(true, options: .notifyOthersOnDeactivation)

        let req = SFSpeechAudioBufferRecognitionRequest()
        req.shouldReportPartialResults = true
        req.taskHint = .dictation
        req.addsPunctuation = true
        // Keep the audio on the phone when the device can recognize it there.
        if recognizer.supportsOnDeviceRecognition {
            req.requiresOnDeviceRecognition = true
        }

        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        let box = level
        input.installTap(onBus: 0, bufferSize: 1024, format: format) { buffer, _ in
            req.append(buffer)
            box.set(Self.level(of: buffer))
        }
        engine.prepare()
        do {
            try engine.start()
        } catch {
            input.removeTap(onBus: 0)
            deactivateSession()
            throw error
        }

        request = req
        text = ""
        recognitionDone = false
        generation += 1
        task = recognizer.recognitionTask(with: req) { [weak self] result, error in
            let partial = result?.bestTranscription.formattedString
            let done = (result?.isFinal ?? false) || error != nil
            Task { @MainActor in self?.handle(partial, done: done) }
        }
        startedAt = Date()
        elapsed = 0
        levels = Array(repeating: 0, count: Self.meterBars)
        isListening = true
        meterTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                self.tick()
                try? await Task.sleep(nanoseconds: 50_000_000)
            }
        }
    }

    /// Stop listening and return the final text (empty if nothing was heard).
    func stop() async -> String {
        guard isListening else { return "" }
        stopAudio()
        isFinishing = true
        request?.endAudio()
        if !recognitionDone {
            // The final result normally follows within a moment; don't wait forever.
            let current = generation
            await withCheckedContinuation { c in
                finished = c
                Task { [weak self] in
                    try? await Task.sleep(nanoseconds: 3_000_000_000)
                    if self?.generation == current { self?.finish() }
                }
            }
        }
        let result = text
        cleanUp()
        return result
    }

    #if DEBUG
    /// Screenshots: look as if listening, with `text` heard so far. Uses no
    /// audio; Discard or Done end it.
    func showDemo(text: String) {
        self.text = text
        elapsed = 7
        levels = (0..<Self.meterBars).map { i in Float(0.15 + 0.7 * abs(sin(Double(i) * 0.9))) }
        isListening = true
        recognitionDone = true
    }
    #endif

    /// Stop and throw the text away.
    func cancel() {
        guard isListening || isFinishing else { return }
        stopAudio()
        task?.cancel()
        finish()
        cleanUp()
        text = ""
    }

    private func handle(_ partial: String?, done: Bool) {
        if let partial, isListening || isFinishing { text = partial }
        guard done else { return }
        recognitionDone = true
        if isFinishing {
            finish()
        } else if isListening {
            // Recognition ended on its own (an error, or a long silence):
            // keep what was heard.
            onAutoStop?()
        }
    }

    private func finish() {
        finished?.resume()
        finished = nil
    }

    private func stopAudio() {
        meterTask?.cancel()
        meterTask = nil
        if engine.isRunning { engine.stop() }
        engine.inputNode.removeTap(onBus: 0)
        isListening = false
        deactivateSession()
    }

    private func cleanUp() {
        task = nil
        request = nil
        isFinishing = false
    }

    private func deactivateSession() {
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }

    private func tick() {
        guard isListening else { return }
        guard engine.isRunning else {
            // Interrupted by the system: keep what was heard.
            onAutoStop?()
            return
        }
        levels.removeFirst()
        levels.append(level.get())
        elapsed = Date().timeIntervalSince(startedAt)
        if elapsed >= Self.maxDuration {
            onAutoStop?()
        }
    }

    /// RMS of the first channel, -50 dB (quiet room) … 0 dB (loud) mapped to 0…1.
    nonisolated private static func level(of buffer: AVAudioPCMBuffer) -> Float {
        guard let samples = buffer.floatChannelData?[0], buffer.frameLength > 0 else { return 0 }
        let n = Int(buffer.frameLength)
        var sum: Float = 0
        for i in 0..<n { sum += samples[i] * samples[i] }
        let rms = (sum / Float(n)).squareRoot()
        let db = 20 * log10(max(rms, 1e-7))
        return max(0, min(1, (db + 50) / 50))
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
                    if let preview = attachment.preview {
                        Image(uiImage: preview)
                            .resizable()
                            .scaledToFill()
                            .frame(width: 32, height: 32)
                            .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
                            .accessibilityHidden(true)
                    } else {
                        Image(systemName: info.isImage ? "photo" : "doc")
                            .foregroundStyle(.secondary)
                    }
                case .failed:
                    Image(systemName: "exclamationmark.circle.fill")
                        .foregroundStyle(.red)
                }
            }
            .frame(width: attachment.preview == nil ? 20 : 32)
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

/// Replaces the prompt row while dictating: the text so far, discard, the
/// time, a level meter and done, on a glass panel.
struct DictationPanel: View {
    @ObservedObject var dictation: SpeechDictation
    var onCancel: @MainActor () -> Void
    var onStop: @MainActor () -> Void

    var body: some View {
        VStack(spacing: 10) {
            Group {
                if dictation.text.isEmpty {
                    Text(dictation.isFinishing ? "Finishing…" : "Listening…")
                        .foregroundStyle(.secondary)
                } else {
                    // The newest words, cut at the start.
                    Text(dictation.text)
                        .foregroundStyle(.primary)
                        .lineLimit(1...3)
                        .truncationMode(.head)
                }
            }
            .frame(maxWidth: .infinity, minHeight: 22, alignment: .leading)
            .padding(.horizontal, 4)

            HStack(spacing: 12) {
                Button(role: .destructive) {
                    onCancel()
                } label: {
                    Image(systemName: "trash")
                        .font(.body.weight(.medium))
                        .foregroundStyle(Palette.red)
                }
                .buttonStyle(GlassCircleButtonStyle(size: 44, tint: Color.white.opacity(0.08)))
                .disabled(dictation.isFinishing)
                .accessibilityLabel("Discard dictation")

                HStack(spacing: 6) {
                    if dictation.isFinishing {
                        ProgressView().controlSize(.small)
                    } else {
                        Circle().fill(Palette.red).frame(width: 7, height: 7)
                    }
                    Text(timeText)
                        .font(.subheadline.weight(.semibold).monospacedDigit())
                        .foregroundStyle(Palette.red)
                }
                .frame(width: 64, alignment: .leading)

                LevelMeter(levels: Array(dictation.levels.suffix(16)))
                    .frame(maxWidth: .infinity)

                Spacer().frame(width: 20)

                Button {
                    onStop()
                } label: {
                    Image(systemName: "checkmark")
                        .font(.body.weight(.bold))
                        .foregroundStyle(.white)
                }
                .buttonStyle(GlassCircleButtonStyle(size: 44, tint: .accentColor))
                .disabled(dictation.isFinishing)
                .accessibilityLabel("Done dictating")
            }

            Text("iOS dictation · on this iPhone")
                .font(.caption2)
                .foregroundStyle(.tertiary)
        }
        .padding(.horizontal, 14)
        .padding(.top, 14)
        .padding(.bottom, 8)
        .glassSurface(RoundedRectangle(cornerRadius: 26, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityLabel(dictation.isFinishing ? "Finishing dictation" : "Dictating, \(timeText)")
    }

    private var timeText: String {
        let s = Int(dictation.elapsed)
        return String(format: "%d:%02d", s / 60, s % 60)
    }
}

/// Bars that follow the input level, centred, newest on the right.
struct LevelMeter: View {
    let levels: [Float]

    var body: some View {
        HStack(alignment: .center, spacing: 3) {
            ForEach(Array(levels.enumerated()), id: \.offset) { i, level in
                Capsule()
                    .fill(Color.white.opacity(i < levels.count - 3 ? 0.9 : 0.45))
                    .frame(width: 2.5, height: 4 + CGFloat(level) * 20)
            }
        }
        .frame(height: 26)
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

// MARK: - Prompt field

/// The prompt field: a `UITextView`, so pasting an image or a file (the
/// edit menu's Paste, or ⌘V) adds it as an attachment, as in Messages.
/// Grows to `maxLines`, then scrolls. Text pastes, dictation and the
/// keyboard behave as in any text view.
struct ComposerTextView: UIViewRepresentable {
    @Binding var text: String
    @Binding var isFocused: Bool
    var placeholder: String
    var isEnabled = true
    var maxLines = 6
    /// Space around the text (the placeholder follows).
    var insets = UIEdgeInsets(top: 11, left: 16, bottom: 11, right: 0)
    /// A leading command shown as a tinted token (`/btw` for a side
    /// question) when the text starts with it.
    var tokenPrefix: String? = nil
    /// Pasted images and files; nil when the host can't take attachments.
    var onPasteItems: (([NSItemProvider]) -> Void)?

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeUIView(context: Context) -> PastingTextView {
        let v = PastingTextView()
        v.delegate = context.coordinator
        v.font = UIFont.preferredFont(forTextStyle: .body)
        v.adjustsFontForContentSizeCategory = true
        v.backgroundColor = .clear
        v.textColor = .label
        v.textContainerInset = insets
        v.textContainer.lineFragmentPadding = 0
        v.isScrollEnabled = false
        v.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        v.accessibilityLabel = placeholder
        return v
    }

    func updateUIView(_ v: PastingTextView, context: Context) {
        context.coordinator.parent = self
        if v.text != text { v.text = text }
        v.isEditable = isEnabled
        v.isSelectable = isEnabled
        if v.textContainerInset != insets { v.textContainerInset = insets }
        v.placeholder = placeholder
        v.accessibilityLabel = placeholder
        v.tokenPrefix = tokenPrefix
        v.onPasteItems = isEnabled ? onPasteItems : nil
        if isFocused, isEnabled, !v.isFirstResponder, v.window != nil {
            DispatchQueue.main.async { v.becomeFirstResponder() }
        } else if !isFocused, v.isFirstResponder {
            DispatchQueue.main.async { v.resignFirstResponder() }
        }
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView v: PastingTextView, context: Context) -> CGSize? {
        let width = proposal.width ?? 200
        let fitting = v.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude))
        let line = v.font?.lineHeight ?? 20
        let maxHeight = ceil(line * CGFloat(maxLines)) + v.textContainerInset.top + v.textContainerInset.bottom
        let scrolls = fitting.height > maxHeight
        if v.isScrollEnabled != scrolls {
            DispatchQueue.main.async { v.isScrollEnabled = scrolls }
        }
        return CGSize(width: width, height: min(fitting.height, maxHeight))
    }

    final class Coordinator: NSObject, UITextViewDelegate {
        var parent: ComposerTextView
        init(_ parent: ComposerTextView) { self.parent = parent }

        func textViewDidChange(_ v: UITextView) {
            parent.text = v.text
            (v as? PastingTextView)?.updatePlaceholder()
            (v as? PastingTextView)?.styleToken()
        }

        func textViewDidBeginEditing(_ v: UITextView) {
            if !parent.isFocused { parent.isFocused = true }
        }

        func textViewDidEndEditing(_ v: UITextView) {
            if parent.isFocused { parent.isFocused = false }
        }
    }
}

/// A text view that takes pasted images and files as attachments.
final class PastingTextView: UITextView {
    var onPasteItems: (([NSItemProvider]) -> Void)?
    private let placeholderLabel = UILabel()

    var placeholder: String = "" {
        didSet { placeholderLabel.text = placeholder }
    }

    override var text: String! {
        didSet {
            updatePlaceholder()
            styleToken()
        }
    }

    var tokenPrefix: String? {
        didSet { if oldValue != tokenPrefix { styleToken() } }
    }

    /// Whether the text starts with the token, alone or before a space.
    private var startsWithToken: Bool {
        guard let p = tokenPrefix, let t = text, t.count >= p.count,
              t.prefix(p.count).lowercased() == p.lowercased() else { return false }
        return t.count == p.count || t[t.index(t.startIndex, offsetBy: p.count)].isWhitespace
    }

    /// The token in the tint colour and semibold; the rest as typed.
    func styleToken() {
        guard markedTextRange == nil else { return }
        let base = font ?? UIFont.preferredFont(forTextStyle: .body)
        let storage = textStorage
        let full = NSRange(location: 0, length: storage.length)
        storage.beginEditing()
        storage.addAttributes([.font: base, .foregroundColor: UIColor.label], range: full)
        if startsWithToken, let p = tokenPrefix {
            let bold = UIFont.systemFont(ofSize: base.pointSize, weight: .semibold)
            storage.addAttributes([.font: bold, .foregroundColor: tintColor ?? .systemBlue],
                                  range: NSRange(location: 0, length: (p as NSString).length))
        }
        storage.endEditing()
        typingAttributes = [.font: base, .foregroundColor: UIColor.label]
    }

    override func tintColorDidChange() {
        super.tintColorDidChange()
        styleToken()
    }

    private var placeholderLeading: NSLayoutConstraint!
    private var placeholderTop: NSLayoutConstraint!

    override var textContainerInset: UIEdgeInsets {
        didSet {
            placeholderLeading?.constant = textContainerInset.left
            placeholderTop?.constant = textContainerInset.top
        }
    }

    override init(frame: CGRect, textContainer: NSTextContainer?) {
        super.init(frame: frame, textContainer: textContainer)
        placeholderLabel.font = UIFont.preferredFont(forTextStyle: .body)
        placeholderLabel.adjustsFontForContentSizeCategory = true
        placeholderLabel.textColor = .placeholderText
        placeholderLabel.isAccessibilityElement = false
        placeholderLabel.translatesAutoresizingMaskIntoConstraints = false
        addSubview(placeholderLabel)
        placeholderLeading = placeholderLabel.leadingAnchor.constraint(
            equalTo: frameLayoutGuide.leadingAnchor, constant: textContainerInset.left)
        placeholderTop = placeholderLabel.topAnchor.constraint(
            equalTo: frameLayoutGuide.topAnchor, constant: textContainerInset.top)
        NSLayoutConstraint.activate([
            placeholderLeading,
            placeholderLabel.trailingAnchor.constraint(lessThanOrEqualTo: frameLayoutGuide.trailingAnchor),
            placeholderTop,
        ])
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    func updatePlaceholder() {
        placeholderLabel.isHidden = !(text ?? "").isEmpty
    }

    /// Something on the pasteboard that becomes an attachment: an image, or
    /// a file with no text form. (Checked without reading the pasteboard,
    /// so iOS shows no paste notice.)
    private var pasteboardHasAttachment: Bool {
        let pb = UIPasteboard.general
        return pb.hasImages || (pb.numberOfItems > 0 && !pb.hasStrings && !pb.hasURLs)
    }

    override func canPerformAction(_ action: Selector, withSender sender: Any?) -> Bool {
        if action == #selector(paste(_:)), onPasteItems != nil, pasteboardHasAttachment {
            return true
        }
        return super.canPerformAction(action, withSender: sender)
    }

    override func paste(_ sender: Any?) {
        guard let onPasteItems, pasteboardHasAttachment else {
            super.paste(sender)
            return
        }
        let pb = UIPasteboard.general
        let providers = pb.itemProviders.filter { p in
            p.hasItemConformingToTypeIdentifier(UTType.image.identifier)
                || (!pb.hasImages && !p.hasItemConformingToTypeIdentifier(UTType.text.identifier))
        }
        if providers.isEmpty {
            super.paste(sender)
        } else {
            onPasteItems(providers)
        }
    }
}

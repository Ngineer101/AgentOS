import Foundation
import UIKit
import UniformTypeIdentifiers

/// Files the agent sent (`agent.file`), downloaded on first tap through
/// `attachment.read` into the caches directory
/// (`Caches/AgentFiles/<session>/<attachment>/<name>`), so a second tap opens
/// them at once. Deleted on unpair, like the transcripts; iOS may also clear
/// the caches directory when space runs low (the file downloads again).
@MainActor
final class AgentFiles {
    static let shared = AgentFiles()

    /// Downloads under way, by local path (once, however many views ask).
    private var loading: [String: Task<URL, Error>] = [:]

    private init() {}

    nonisolated static var root: URL {
        FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("AgentFiles", isDirectory: true)
    }

    /// Where a session's attachment is (or will be) kept, under its own name
    /// so QuickLook, the player and Share see the right name and type.
    nonisolated static func localURL(sessionId: String, attachment a: AttachmentInfo) -> URL {
        root.appendingPathComponent(safe(sessionId), isDirectory: true)
            .appendingPathComponent(safe(a.attachmentId), isDirectory: true)
            .appendingPathComponent(fileName(a))
    }

    /// The local copy, if it was downloaded before.
    nonisolated static func cached(sessionId: String, attachment a: AttachmentInfo) -> URL? {
        let url = localURL(sessionId: sessionId, attachment: a)
        return FileManager.default.fileExists(atPath: url.path) ? url : nil
    }

    /// The local copy, downloaded with `fetch` if there is none yet.
    func file(sessionId: String, attachment a: AttachmentInfo,
              fetch: @escaping () async throws -> Data) async throws -> URL {
        let dest = Self.localURL(sessionId: sessionId, attachment: a)
        if FileManager.default.fileExists(atPath: dest.path) { return dest }
        let key = dest.path
        if let task = loading[key] { return try await task.value }
        let task = Task<URL, Error> {
            let data = try await fetch()
            try await Task.detached(priority: .utility) {
                try AgentFiles.write(data, to: dest)
            }.value
            return dest
        }
        loading[key] = task
        defer { loading[key] = nil }
        return try await task.value
    }

    nonisolated static func deleteAll() {
        try? FileManager.default.removeItem(at: root)
    }

    nonisolated private static func write(_ data: Data, to dest: URL) throws {
        try FileManager.default.createDirectory(
            at: dest.deletingLastPathComponent(),
            withIntermediateDirectories: true,
            attributes: [.protectionKey: FileProtectionType.complete]
        )
        try data.write(to: dest, options: [.atomic, .completeFileProtection])
    }

    nonisolated private static func safe(_ s: String) -> String {
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-_"))
        let out = String(s.unicodeScalars.map { allowed.contains($0) ? Character($0) : "_" })
        return out.isEmpty ? "_" : out
    }

    /// The attachment's own name, made safe for a path, with an extension
    /// from its type when it has none.
    nonisolated private static func fileName(_ a: AttachmentInfo) -> String {
        var name = a.name
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: ":", with: "_")
        while name.hasPrefix(".") { name.removeFirst() }
        if name.isEmpty { name = "File" }
        if (name as NSString).pathExtension.isEmpty,
           let ext = UTType(mimeType: a.mime)?.preferredFilenameExtension {
            name += "." + ext
        }
        return name
    }
}

extension AppModel {
    /// An image the agent sent, or a video's poster: from memory, the local
    /// cache or the host (see `SentImages`).
    func agentImage(sessionId: String, attachment a: AttachmentInfo) async throws -> UIImage {
        #if DEBUG
        if isDemo { return DemoData.agentImage(a.attachmentId) }
        #endif
        return try await SentImages.shared.image(a.attachmentId) { [weak self] in
            guard let self else { throw CancellationError() }
            return try await self.readAttachment(sessionId: sessionId, attachmentId: a.attachmentId)
        }
    }

    /// A file the agent sent, on this phone: downloaded the first time, with
    /// `progress` (0...1) as it comes.
    func agentFile(sessionId: String, attachment a: AttachmentInfo,
                   progress: @escaping (Double) -> Void) async throws -> URL {
        if let url = AgentFiles.cached(sessionId: sessionId, attachment: a) { return url }
        #if DEBUG
        if isDemo {
            guard let data = DemoData.agentFileData(a.attachmentId) else {
                throw ComposerError(message: DemoData.noRecordingMessage)
            }
            return try await AgentFiles.shared.file(sessionId: sessionId, attachment: a) { data }
        }
        #endif
        return try await AgentFiles.shared.file(sessionId: sessionId, attachment: a) { [weak self] in
            guard let self else { throw CancellationError() }
            return try await self.readAttachment(sessionId: sessionId, attachmentId: a.attachmentId) { done, total in
                progress(total > 0 ? Double(done) / Double(total) : 0)
            }
        }
    }
}

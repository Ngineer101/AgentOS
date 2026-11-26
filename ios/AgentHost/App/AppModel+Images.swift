import SwiftUI
import UIKit

/// Images sent with prompts, shown again in the transcript. Hosts on
/// protocol 7 keep them (`attachment.read`); this device keeps a copy in its
/// local cache (`AgentHostCache/images`, deleted on unpair like the
/// transcripts) and the decoded images in memory. The phone's own images go
/// into the cache as they upload, so they show at once.
@MainActor
final class SentImages {
    static let shared = SentImages()

    private let memory = NSCache<NSString, UIImage>()
    private var loading: [String: Task<UIImage, Error>] = [:]

    private init() {
        memory.countLimit = 80
    }

    /// Already decoded, for a first frame without a placeholder.
    func decoded(_ attachmentId: String) -> UIImage? {
        memory.object(forKey: attachmentId as NSString)
    }

    /// From memory, the local cache, or `fetch` (once, however many views ask).
    func image(_ attachmentId: String, fetch: @escaping () async throws -> Data) async throws -> UIImage {
        if let img = decoded(attachmentId) { return img }
        if let task = loading[attachmentId] { return try await task.value }
        let task = Task<UIImage, Error> {
            var fetched = false
            var data = await Task.detached(priority: .userInitiated) {
                TranscriptCache.loadImage(attachmentId)
            }.value
            if data == nil {
                data = try await fetch()
                fetched = true
            }
            let bytes = data ?? Data()
            let img = await Task.detached(priority: .userInitiated) { () -> UIImage? in
                UIImage(data: bytes)?.preparingForDisplay()
            }.value
            guard let img else { throw ComposerError(message: "This image can't be shown.") }
            if fetched {
                await Task.detached(priority: .utility) {
                    TranscriptCache.saveImage(bytes, attachmentId: attachmentId)
                }.value
            }
            return img
        }
        loading[attachmentId] = task
        defer { loading[attachmentId] = nil }
        let img = try await task.value
        memory.setObject(img, forKey: attachmentId as NSString)
        return img
    }

    func clearMemory() {
        memory.removeAllObjects()
    }
}

extension AppModel {
    /// Whether the host gives back images sent with prompts.
    var hostKeepsImages: Bool {
        (hostProtocolVersion ?? hostInfo?.protocolVersion ?? 0) >= RPC.imageProtocolVersion
    }

    /// An image sent with one of a session's prompts.
    func sentImage(sessionId: String, attachment a: AttachmentInfo) async throws -> UIImage {
        #if DEBUG
        if isDemo { return DemoData.pastedImage() }
        #endif
        let keeps = hostKeepsImages
        return try await SentImages.shared.image(a.attachmentId) { [weak self] in
            guard let self, keeps else {
                throw ComposerError(message: "Update agenthostd on your computer to see sent images again.")
            }
            return try await self.readAttachment(sessionId: sessionId, attachmentId: a.attachmentId)
        }
    }

    /// The whole file, read from the host in pieces. `progress` gets the
    /// bytes read so far and the file's size after each piece.
    func readAttachment(sessionId: String, attachmentId: String,
                        progress: ((Int, Int) -> Void)? = nil) async throws -> Data {
        var data = Data()
        while true {
            let r: AttachmentReadResult = try await call(
                RPC.attachmentRead,
                ["session_id": sessionId, "attachment_id": attachmentId, "offset": data.count]
            )
            if r.size == 0 { return data }
            guard let piece = Data(base64Encoded: r.data), !piece.isEmpty, data.count + piece.count <= r.size else {
                throw ComposerError(message: "The host sent \(data.count) of \(r.size) bytes.")
            }
            data.append(piece)
            progress?(data.count, r.size)
            if data.count >= r.size { return data }
            try Task.checkCancellation()
        }
    }
}

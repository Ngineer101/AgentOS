import Foundation

/// What the app shows offline: the last known host state and transcripts.
struct CachedState: Codable {
    var hostInfo: HostInfoModel?
    var workspaces: [WorkspaceInfo]
    var sessions: [SessionInfo]
    var deviceName: String?
    /// Sessions the user opened; re-subscribed after every reconnect.
    var watched: [String]
    var savedAt: Date
}

/// Local cache in Application Support, written with
/// `FileProtectionType.complete` (unreadable while the phone is locked) and
/// excluded from backups. Deleted on unpair or revocation.
enum TranscriptCache {
    private static var root: URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return base.appendingPathComponent("AgentHostCache", isDirectory: true)
    }

    private static var transcriptsDir: URL {
        root.appendingPathComponent("transcripts", isDirectory: true)
    }

    private static var stateFile: URL {
        root.appendingPathComponent("state.json")
    }

    /// Images sent with prompts, by attachment id (see `SentImages`).
    private static var imagesDir: URL {
        root.appendingPathComponent("images", isDirectory: true)
    }

    private static func ensureDirs() throws {
        let fm = FileManager.default
        for dir in [root, transcriptsDir, imagesDir] where !fm.fileExists(atPath: dir.path) {
            try fm.createDirectory(
                at: dir,
                withIntermediateDirectories: true,
                attributes: [.protectionKey: FileProtectionType.complete]
            )
        }
        var r = root
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? r.setResourceValues(values)
    }

    private static func write<T: Encodable>(_ value: T, to url: URL) throws {
        try ensureDirs()
        let data = try JSONEncoder().encode(value)
        try data.write(to: url, options: [.atomic, .completeFileProtection])
    }

    private static func fileName(_ sessionId: String) -> String {
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-_"))
        let safe = String(sessionId.unicodeScalars.map { allowed.contains($0) ? Character($0) : "_" })
        return safe + ".json"
    }

    static func saveState(_ s: CachedState) {
        try? write(s, to: stateFile)
    }

    static func loadState() -> CachedState? {
        guard let data = try? Data(contentsOf: stateFile) else { return nil }
        return try? JSONDecoder().decode(CachedState.self, from: data)
    }

    static func saveTranscript(_ t: Transcript) {
        try? write(t, to: transcriptsDir.appendingPathComponent(fileName(t.sessionId)))
    }

    static func loadTranscripts() -> [String: Transcript] {
        let fm = FileManager.default
        guard let files = try? fm.contentsOfDirectory(at: transcriptsDir, includingPropertiesForKeys: nil) else {
            return [:]
        }
        var out: [String: Transcript] = [:]
        for f in files where f.pathExtension == "json" {
            if let data = try? Data(contentsOf: f),
               let t = try? JSONDecoder().decode(Transcript.self, from: data) {
                out[t.sessionId] = t
            }
        }
        return out
    }

    private static func imageURL(_ attachmentId: String) -> URL {
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-_"))
        let safe = String(attachmentId.unicodeScalars.map { allowed.contains($0) ? Character($0) : "_" })
        return imagesDir.appendingPathComponent(safe)
    }

    static func saveImage(_ data: Data, attachmentId: String) {
        try? ensureDirs()
        try? data.write(to: imageURL(attachmentId), options: [.atomic, .completeFileProtection])
    }

    static func loadImage(_ attachmentId: String) -> Data? {
        try? Data(contentsOf: imageURL(attachmentId))
    }

    static func deleteAll() {
        try? FileManager.default.removeItem(at: root)
    }
}

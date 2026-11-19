import UserNotifications

/// Opens a notification's encrypted content (`e`) with the key this iPhone
/// registered (`PushKeyStore`) and shows its title and text. Apple only ever
/// sees the generic "A session needs you", which is shown unchanged when
/// anything here fails (no key, wrong key, unknown version).
final class NotificationService: UNNotificationServiceExtension {
    private var contentHandler: ((UNNotificationContent) -> Void)?
    /// The generic alert, delivered if time runs out.
    private var generic: UNNotificationContent?

    override func didReceive(
        _ request: UNNotificationRequest,
        withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void
    ) {
        self.contentHandler = contentHandler
        generic = request.content
        let info = request.content.userInfo
        let version = (info["v"] as? NSNumber)?.intValue ?? 0
        guard version == PushCrypto.version,
              let sealed = info["e"] as? String,
              let key = PushKeyStore.load(),
              let content = try? PushCrypto.decrypt(sealed, key: key),
              let shown = request.content.mutableCopy() as? UNMutableNotificationContent
        else {
            finish(request.content)
            return
        }
        shown.title = content.title
        shown.body = content.body
        var userInfo = shown.userInfo
        // The sealed copy is not needed once opened.
        userInfo["e"] = nil
        userInfo[PushUserInfo.kind] = content.kind
        if let sid = content.sessionId {
            shown.threadIdentifier = sid
            userInfo[PushUserInfo.sessionId] = sid
        }
        if let aid = content.approvalId {
            userInfo[PushUserInfo.approvalId] = aid
        }
        shown.userInfo = userInfo
        finish(shown)
    }

    override func serviceExtensionTimeWillExpire() {
        if let generic { finish(generic) }
    }

    private func finish(_ content: UNNotificationContent) {
        guard let handler = contentHandler else { return }
        contentHandler = nil
        handler(content)
    }
}

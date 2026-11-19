import Foundation

/// Screen sharing: start the host's screen-relay, then open a stream on a
/// tunnel of its own (`ScreenSession`), so frames never hold up approvals.
extension AppModel {
    var hostSharesScreen: Bool {
        (hostProtocolVersion ?? hostInfo?.protocolVersion ?? 0) >= RPC.screenProtocolVersion
    }

    func refreshScreen() async {
        guard hostSharesScreen else {
            screenStatus = nil
            return
        }
        #if DEBUG
        if isDemo { return }
        #endif
        if let st: ScreenStatusModel = try? await call(RPC.screenStatus, [:]) {
            screenStatus = st
        }
    }

    /// `screen.start` on the main connection, then `screen.attach` with the
    /// ticket on a new tunnel. Frames go to `listener`.
    func openScreen(mode: String, control: Bool, listener: ScreenListener) async throws -> ScreenSession {
        guard let creds = credentials else { throw AgentHostError.NotConnected }
        let start: ScreenStartResultModel = try await call(
            RPC.screenStart,
            ["mode": mode, "control": control]
        )
        return try await ScreenSession.open(credentials: creds.ffi, ticket: start.ticket, listener: listener)
    }

    func stopScreen() async {
        #if DEBUG
        if isDemo { return }
        #endif
        try? await send(RPC.screenStop, [:])
    }
}

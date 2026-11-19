import MoqFFI

extension MoqError {
    /// True for `Cancelled` and `Closed`, which arise from graceful shutdown
    /// rather than actual failures. Useful for swallowing the expected error
    /// an `AsyncSequence` produces when its consuming task is cancelled.
    public var isShutdown: Bool {
        moqErrorIsShutdown(err: self)
    }

    /// True for HTTP 401/403, an expired or malformed token, a protocol Unauthorized or Expired session close,
    /// and a track revocation. Unlike a transport failure, retrying without new credentials won't help, so callers
    /// should surface these rather than reconnect.
    public var isAuth: Bool {
        moqErrorIsAuth(err: self)
    }

    /// The structured protocol failure, or nil if this is not one.
    ///
    /// Carries the peer's session or stream scope, the verbatim wire code, a known
    /// kind when recognized, and a diagnostic message.
    public var protocolError: MoqProtocolError? {
        switch self {
        case .Protocol(let details): return details
        default: return nil
        }
    }
}

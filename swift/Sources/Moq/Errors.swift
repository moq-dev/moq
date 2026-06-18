import MoqFFI

extension MoqError {
    /// True for `Cancelled` and `Closed`, which arise from graceful shutdown
    /// rather than actual failures. Useful for swallowing the expected error
    /// an `AsyncSequence` produces when its consuming task is cancelled.
    public var isShutdown: Bool {
        switch self {
        case .Cancelled, .Closed: return true
        default: return false
        }
    }

    /// True for `Unauthorized` (HTTP 401) and `Forbidden` (HTTP 403), which the
    /// server returns to reject the connection on authentication or authorization
    /// grounds. Unlike a transport failure, retrying without new credentials won't
    /// help, so callers should surface these rather than reconnect.
    public var isAuth: Bool {
        switch self {
        case .Unauthorized, .Forbidden: return true
        default: return false
        }
    }
}

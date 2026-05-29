import Foundation
@_exported import MoqFFI

extension MoqError {
    /// True for `Cancelled` and `Closed`, which arise from graceful shutdown
    /// rather than actual failures. Useful for swallowing the expected error
    /// that an `AsyncSequence` produces when its consumer cancels.
    public var isShutdown: Bool {
        switch self {
        case .Cancelled, .Closed: return true
        default: return false
        }
    }
}

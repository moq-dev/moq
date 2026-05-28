import Foundation
@_exported import MoqFFI

extension MoqSession {
    /// Graceful close. Documents the convention that code 0 means "no error".
    public func close() {
        cancel(code: 0)
    }
}

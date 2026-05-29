import Foundation
@_exported import MoqFFI

extension MoqSession {
    /// Suspend until the session is closed.
    public func waitForClose() async throws {
        try await closed()
    }
}

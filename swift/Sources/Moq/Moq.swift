import Foundation
@_exported import MoqFFI

/// Top-level entry points for the Moq protocol stack.
public enum Moq {
    /// Connect with a single origin attached as both publish source and
    /// consume sink. The returned origin is what you use to publish local
    /// broadcasts and to discover remote announcements.
    ///
    /// Thin Swift-idiomatic wrapper over `MoqFFI.connect(url:)`: the FFI
    /// returns a `MoqClientSession` record, this destructures it into a
    /// tuple so callers can `let (session, origin) = ...`.
    public static func connect(url: String) async throws -> (MoqSession, MoqOriginProducer) {
        let result = try await MoqFFI.connect(url: url)
        return (result.session, result.origin)
    }

    /// Build a client with custom configuration before connecting.
    /// Use when you need TLS / bind options or a non-duplex topology
    /// (only consume, only publish, separate origins).
    public static func client() -> MoqClient {
        MoqClient()
    }
}

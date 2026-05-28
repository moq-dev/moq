import Foundation
@_exported import MoqFFI

/// Top-level entry points for the Moq protocol stack.
public enum Moq {
    /// Connect with a single origin attached as both publish source and
    /// consume sink, then return both. Convenience over
    /// `MoqClient().connectDuplex(url:)` that destructures the FFI record
    /// into a tuple so callers can `let (session, origin) = ...`.
    ///
    /// For custom TLS / bind options, build a client via `Moq.client()`,
    /// configure it, then call `connectDuplex(url:)` on it.
    public static func connect(url: String) async throws -> (MoqSession, MoqOriginProducer) {
        let result = try await MoqClient().connectDuplex(url: url)
        return (result.session, result.origin)
    }

    /// Build a client with custom configuration before connecting.
    /// Pair with `connectDuplex(url:)` for the auto-wired duplex setup,
    /// or with `connect(url:)` and your own `setPublish` / `setConsume`
    /// for a custom topology.
    public static func client() -> MoqClient {
        MoqClient()
    }
}

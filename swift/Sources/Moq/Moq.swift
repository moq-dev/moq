import Foundation
@_exported import MoqFFI

/// Top-level entry points for the Moq protocol stack.
public enum Moq {
    /// Connect with a single origin attached as both publish source and
    /// consume sink. The returned origin is what you use to publish local
    /// broadcasts and to discover remote announcements.
    public static func connect(url: String) async throws -> (MoqSession, MoqOriginProducer) {
        let origin = MoqOriginProducer()
        let client = MoqClient()
        client.setPublish(origin: origin)
        client.setConsume(origin: origin)
        let session = try await client.connect(url: url)
        return (session, origin)
    }

    /// Build a client with custom configuration before connecting.
    /// Use when you need TLS / bind options or a non-duplex topology
    /// (only consume, only publish, separate origins).
    public static func client() -> MoqClient {
        MoqClient()
    }
}

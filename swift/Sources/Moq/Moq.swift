import Foundation
import MoqFFI

/// Top-level entry points for the Moq protocol stack.
public enum Moq {
    /// Connect to a MoQ relay using default client configuration.
    public static func connect(url: String) async throws -> MoqSession {
        let client = MoqClient()
        return try await client.connect(url: url)
    }

    /// Build a client with custom configuration before connecting.
    public static func client() -> MoqClient {
        MoqClient()
    }
}

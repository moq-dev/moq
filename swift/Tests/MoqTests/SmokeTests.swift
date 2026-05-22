import Foundation
import XCTest
@testable import Moq
import MoqFFI

final class SmokeTests: XCTestCase {
    /// Verifies the native lib loads and the wrapper compiles against
    /// the generated API. No network needed: we just instantiate a few
    /// types and exercise the cancel path.
    func testClientConstructsAndCancels() async throws {
        let client = MoqClient()
        client.cancel()
        do {
            _ = try await client.connect(url: "https://localhost:0/test")
            XCTFail("expected error from cancelled client")
        } catch let error as MoqError {
            XCTAssertTrue(
                error.isShutdown ||
                    {
                        if case .Connect = error { return true } else { return false }
                    }() ||
                    {
                        if case .Url = error { return true } else { return false }
                    }(),
                "expected shutdown/connect/url error, got: \(error)"
            )
        }
    }

    func testOriginProducerIsConstructible() {
        let origin = MoqOriginProducer()
        _ = origin.consume()
    }
}

import Foundation
import XCTest
@testable import Moq

final class SmokeTests: XCTestCase {
    /// Verifies the native lib loads and the wrapper compiles against the
    /// generated API. No network needed: we just instantiate a few types and
    /// exercise the cancel path.
    func testClientConstructsAndCancels() async throws {
        let client = Client()
        client.cancel()
        do {
            _ = try await client.connect(to: "https://localhost:0/test")
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
        let origin = OriginProducer()
        _ = origin.consume()
    }

    func testBroadcastProducerOpensTracks() throws {
        let broadcast = try BroadcastProducer()
        let track = try broadcast.publishTrack(name: "events")
        XCTAssertEqual(try track.name, "events")
        try track.finish()
        try broadcast.finish()
    }
}

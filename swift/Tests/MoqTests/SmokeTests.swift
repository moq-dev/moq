import Foundation
import XCTest
@testable import Moq

final class SmokeTests: XCTestCase {
    /// Verifies the native lib loads and the wrapper compiles against the
    /// generated API. No network needed: we just instantiate a few types and
    /// exercise the cancel path.
    func testClientConstructsAndCancels() async throws {
        let client = Client()
        client.setTlsRoots([])
        client.setTlsSystemRoots(true)
        client.setTlsFingerprints([])
        client.setTlsCert(nil)
        client.setTlsKey(nil)
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
        let origin = OriginProducer(cacheCapacityBytes: 4096)
        _ = origin.consume()
        _ = origin.dynamic()
    }

    func testBroadcastProducerOpensTracks() throws {
        let broadcast = try BroadcastProducer()
        let track = try broadcast.publishTrack(name: "events")
        XCTAssertEqual(try track.name, "events")
        try track.finish()
        try broadcast.finish()
    }

    func testVideoHintsReachMediaPublishApi() throws {
        let broadcast = try BroadcastProducer()
        let hint = VideoHint(
            coded: Dimensions(width: 1920, height: 1080),
            displayAspect: nil,
            bitrate: 4_000_000,
            framerate: 60,
            optimizeForLatency: true
        )
        let media = try broadcast.publishMedia(format: "avc3", video: hint)
        try media.finish()
        try broadcast.finish()
    }

    func testBroadcastConsumerFetchesCachedGroup() async throws {
        let broadcast = try BroadcastProducer()
        let track = try broadcast.publishTrack(name: "events")
        let group = try track.appendGroup()
        try group.writeFrame(Data("cached".utf8), timestampUs: 0)
        try group.finish()

        let consumer = try broadcast.consume()
        let fetched = try await consumer.fetchGroup(
            name: "events",
            sequence: 0,
            options: FetchGroupOptions(priority: 3)
        )
        XCTAssertEqual(fetched.sequence, 0)
        let frame = try await fetched.readFrame()
        XCTAssertEqual(frame?.payload, Data("cached".utf8))
        let end = try await fetched.readFrame()
        XCTAssertNil(end)
    }

    func testJsonSnapshotRoundTrip() async throws {
        struct Status: Codable, Equatable {
            let state: String
            let viewers: Int
        }

        let broadcast = try BroadcastProducer()
        let producer = try broadcast.publishJsonSnapshot(name: "status", of: Status.self, compression: true)
        let consumer = try await broadcast.consume().subscribeJsonSnapshot(
            name: "status", as: Status.self, compression: true)

        try producer.update(Status(state: "live", viewers: 42))
        let first = try await consumer.next()
        XCTAssertEqual(first, Status(state: "live", viewers: 42))

        // A second update supersedes the first; a late reader collapses to the latest.
        try producer.update(Status(state: "live", viewers: 43))
        let second = try await consumer.next()
        XCTAssertEqual(second, Status(state: "live", viewers: 43))

        consumer.cancel()
        try producer.finish()
        try broadcast.finish()
    }

    func testJsonStreamRoundTrip() async throws {
        struct Event: Codable, Equatable {
            let n: Int
        }

        let broadcast = try BroadcastProducer()
        let producer = try broadcast.publishJsonStream(name: "events", of: Event.self)
        let consumer = try await broadcast.consume().subscribeJsonStream(
            name: "events", as: Event.self)

        for n in 0..<3 {
            try producer.append(Event(n: n))
            let record = try await consumer.next()
            XCTAssertEqual(record, Event(n: n))
        }

        consumer.cancel()
        try producer.finish()
        try broadcast.finish()
    }

    func testRawTrackTimestamps() async throws {
        let broadcast = try BroadcastProducer()
        let track = try broadcast.publishTrack(name: "events")
        let consumer = try track.consume()

        let payload = Data("ready".utf8)
        try track.writeFrame(payload, timestampUs: 12_345)

        let frame = try await consumer.readFrame()
        XCTAssertEqual(frame?.payload, payload)
        XCTAssertEqual(frame?.timestampUs, 12_345)

        let group = try track.appendGroup()
        let groupConsumer = try group.consume()
        let groupPayload = Data("group".utf8)
        try group.writeFrame(groupPayload, timestampUs: 23_456)
        try group.finish()

        let groupFrame = try await groupConsumer.readFrame()
        XCTAssertEqual(groupFrame?.payload, groupPayload)
        XCTAssertEqual(groupFrame?.timestampUs, 23_456)

        try track.finish()
        try broadcast.finish()
    }

    func testSparseGroupsAndKnownEnd() throws {
        let broadcast = try BroadcastProducer()
        let track = try broadcast.publishTrack(name: "sparse")
        let group = try track.createGroup(sequence: 2)
        XCTAssertEqual(group.sequence, 2)
        try group.finish()

        try track.finish(at: 5)
        try track.createGroup(sequence: 4).finish()
        XCTAssertThrowsError(try track.createGroup(sequence: 5))
        try track.finish()
        try broadcast.finish()
    }
}

import Foundation
import XCTest
@testable import Moq

/// The next announce event that is not `.live`, which lands wherever the backlog ends.
private func nextRoute(_ announced: AnnounceConsumer) async throws -> AnnounceEvent {
    while true {
        let event = try XCTUnwrap(try await announced.next(), "announce stream ended")
        if event != .live {
            return event
        }
    }
}

final class SmokeTests: XCTestCase {
    func testStreamAbortPreservesProtocolDetails() async throws {
        let broadcast = try BroadcastProducer()
        let track = try broadcast.publishTrack(name: "errors")
        let producer = try track.appendGroup()
        let consumer = try broadcast.consume()
        let group = try await consumer.fetchGroup(name: "errors", sequence: 0)
        try producer.abort(errorCode: 404)
        do {
            _ = try await group.readFrame()
            XCTFail("expected a protocol error")
        } catch let error as MoqError {
            let details = try XCTUnwrap(error.protocolError)
            XCTAssertEqual(details.scope, .stream)
            XCTAssertEqual(details.code, 468)
            XCTAssertEqual(details.kind, .app)
        }
    }

    /// Verifies the native lib loads and the wrapper compiles against the
    /// generated API. No network needed: we just instantiate a few types and
    /// exercise the cancel path.
    func testClientConstructsAndCancels() async throws {
        let client = Client()
        try client.setTlsRoots([])
        try client.setTlsSystemRoots(true)
        try client.setTlsFingerprints([])
        try client.setTlsCert(nil)
        try client.setTlsKey(nil)
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

    /// `cancel()` releases the listening socket before it returns, so the same
    /// address binds again with no retry.
    func testServerCloseReleasesPort() async throws {
        let first = Server()
        try first.bind("127.0.0.1:0")
        try first.generateTls(hostnames: ["localhost"])
        let addr = try await first.listen()
        first.cancel()

        // No retry: cancel() closed the socket, so this binds on the first try.
        let second = Server()
        try second.bind(addr)
        try second.generateTls(hostnames: ["localhost"])
        let rebound = try await second.listen()
        XCTAssertEqual(rebound, addr)
        second.cancel()
    }

    func testOriginProducerIsConstructible() throws {
        let origin = OriginProducer(cacheCapacityBytes: 4096)
        _ = origin.consume()
        _ = try origin.dynamic(prefix: "")
    }

    func testBroadcastIsReachableOnlyWhileAnnounced() async throws {
        let origin = OriginProducer()
        let broadcast = try origin.createBroadcast(path: "live")
        _ = try broadcast.publishTrack(name: "events")
        let consumer = origin.consume()
        do {
            _ = try await consumer.requestBroadcast(path: "live")
            XCTFail("an unannounced broadcast must be unroutable")
        } catch {}

        try broadcast.announce()
        let announced = try consumer.announced(prefix: "")
        guard case .announced(let first) = try await nextRoute(announced) else {
            return XCTFail("expected an announcement")
        }
        XCTAssertEqual(first.prefix, "live")

        try broadcast.unannounce()
        guard case .retracted(let retracted) = try await nextRoute(announced) else {
            return XCTFail("expected a retraction")
        }
        XCTAssertEqual(retracted.prefix, "live")
        do {
            _ = try await consumer.requestBroadcast(path: "live")
            XCTFail("an unannounced broadcast must be unroutable")
        } catch {}

        try broadcast.announce()
        guard case .announced = try await nextRoute(announced) else {
            return XCTFail("expected a reannouncement")
        }
        _ = try await consumer.requestBroadcast(path: "live")
    }

    func testAnnouncedPatternCaptures() async throws {
        let origin = OriginProducer()
        let announced = try origin.consume().announced(prefix: "room", filter: "*/chat")
        let chat = try origin.createBroadcast(path: "room/alice/chat")
        try chat.announce()

        guard case .announced(let update) = try await nextRoute(announced) else {
            return XCTFail("expected an announcement")
        }
        XCTAssertEqual(update.prefix, "room/alice/chat")
        XCTAssertEqual(update.captures, ["alice"])
    }

    func testAnnouncedYieldsLiveOnceCaughtUp() async throws {
        let origin = OriginProducer()
        let consumer = origin.consume()

        let empty = try consumer.announced()
        let first = try await empty.next()
        XCTAssertEqual(first, .live)
        empty.cancel()

        let broadcast = try origin.createBroadcast(path: "cam")
        try broadcast.announce()
        _ = try await consumer.announcedBroadcast(path: "cam").available()

        var listed: [String] = []
        for try await event in try consumer.announced() {
            if case .announced(let announce) = event {
                listed.append(announce.prefix)
            } else if event == .live {
                break
            }
        }
        XCTAssertEqual(listed, ["cam"])
    }

    func testDynamicServesARequestUnderAPrefix() async throws {
        let origin = OriginProducer()
        let dynamic = try origin.dynamic(prefix: "live")
        let pending = Task {
            try await origin.consume().requestBroadcast(path: "live/cam")
        }
        let request = try await dynamic.requestedBroadcast()
        XCTAssertEqual(try request.path, "live/cam")
        let served = try BroadcastProducer()
        try request.accept(broadcast: served)
        _ = try await pending.value
        dynamic.cancel()
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
            bitrate: 4_000_000,
            framerate: 60,
            optimizeForLatency: true
        )
        let media = try broadcast.publishVideo(format: .avc3, hint: hint)
        try media.finish()
        try broadcast.finish()
    }

    func testVideoPropertiesUseDefaultedFields() throws {
        let broadcast = try BroadcastProducer()
        try broadcast.setVideoProperties(VideoProperties(rotation: 315))
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

    func testJsonProducersReportDemand() async throws {
        let broadcast = try BroadcastProducer()
        let snapshot = try broadcast.publishJsonSnapshot(name: "status", of: [String: Int].self)
        let stream = try broadcast.publishJsonStream(name: "events", of: [String: Int].self)
        let snapshotDemand = try snapshot.demand()
        let streamDemand = try stream.demand()
        XCTAssertEqual(snapshotDemand.name, "status")
        XCTAssertFalse(snapshotDemand.isUsed)
        let consumer = try broadcast.consume()

        let snapshotConsumer = try await consumer.subscribeJsonSnapshot(name: "status", as: [String: Int].self)
        let streamConsumer = try await consumer.subscribeJsonStream(name: "events", as: [String: Int].self)
        try await snapshotDemand.used()
        try await streamDemand.used()
        XCTAssertTrue(snapshotDemand.isUsed)

        snapshotConsumer.cancel()
        streamConsumer.cancel()
        try await snapshotDemand.unused()
        try await streamDemand.unused()

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

    func testReadFrameSkipsEmptyThenPopulatedGroups() async throws {
        let broadcast = try BroadcastProducer()
        let track = try broadcast.publishTrack(name: "status")
        let consumer = try track.consume()

        try track.appendGroup().finish()
        try track.appendGroup().finish()
        try track.writeFrame(Data("populated".utf8), timestampUs: 2_000)

        let frame = try await consumer.readFrame()
        XCTAssertEqual(frame?.payload, Data("populated".utf8))
        XCTAssertEqual(frame?.timestampUs, 2_000)

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

    /// `frameDurationUs` is microseconds so Opus' 2.5 ms frame is expressible at
    /// all, and a duration outside the Opus set is refused rather than silently
    /// rounded.
    func testEncodeAudioFrameDurations() throws {
        let broadcast = try BroadcastProducer()
        let input = AudioEncoderInput(format: .f32, sampleRate: 48_000, channels: 1)

        let fine = try broadcast.encodeAudio(
            name: "fine",
            input: input,
            output: AudioEncoderOutput(codec: AudioCodec.opus(), frameDurationUs: 2_500)
        )
        // 2.5 ms of silence at 48 kHz mono f32: exactly one encoded frame.
        try fine.write(AudioFrame(timestampUs: 0, data: Data(count: 120 * 4)))
        try fine.finish()

        XCTAssertThrowsError(
            try broadcast.encodeAudio(
                name: "coarse",
                input: input,
                output: AudioEncoderOutput(codec: AudioCodec.opus(), frameDurationUs: 2_000)
            )
        ) { error in
            if let audio = error as? MoqError, case .Audio = audio { return }
            XCTFail("2 ms is not an opus frame duration: \(error)")
        }

        try broadcast.finish()
    }

    /// A decoded frame owns its picture: it converts to either CPU layout on
    /// demand and stays readable after its consumer is cancelled.
    func testDecodeVideoFrame() async throws {
        let origin = OriginProducer()
        let broadcast = try origin.createBroadcast(path: "video-decode-frame")
        let video = try broadcast.encodeVideo(
            input: VideoEncoderInput(format: .rgba, width: 320, height: 240, framerate: 30),
            // Software so the encode is deterministic everywhere.
            output: VideoEncoderOutput(codec: .h264, track: "camera", kind: .software)
        )
        try broadcast.announce()

        // Seed the track so a subscriber joining below lands on encoded media.
        let rgba = Data(repeating: 0x80, count: 320 * 240 * 4)
        try video.cut()
        for i in 0..<10 {
            try video.write(VideoFrame(timestampUs: UInt64(i) * 33_333, data: rgba))
        }

        let consumer = try await origin.consume().requestBroadcast(path: "video-decode-frame")
        let catalogs = try await consumer.subscribeCatalog()
        // XCTUnwrap takes an autoclosure, which can't hold an await.
        let nextCatalog = try await catalogs.next()
        let catalog = try XCTUnwrap(nextCatalog)
        let rendition = try XCTUnwrap(catalog.video["camera"])

        let decoder = try await consumer.decodeVideo(name: "camera", catalogVideo: rendition)

        // Keep the encoder fed so the decoder sees frames after it joined.
        for i in 10..<40 {
            try video.write(VideoFrame(timestampUs: UInt64(i) * 33_333, data: rgba))
        }

        let next = try await decoder.next()
        let frame = try XCTUnwrap(next)
        decoder.cancel()

        let planar = try frame.pixels(format: .i420)
        XCTAssertEqual(planar.count, Int(frame.width()) * Int(frame.height()) * 3 / 2)

        let packed = try frame.pixels(format: .rgba)
        XCTAssertEqual(packed.count, Int(frame.width()) * Int(frame.height()) * 4)
        XCTAssertTrue(stride(from: 3, to: packed.count, by: 4).allSatisfy { packed[$0] == 0xFF })

        try video.finish()
        try broadcast.finish()
    }

    func testEncodeAudioWithOpusObject() throws {
        // The config retains the codec, so releasing either first must still encode.
        let input = AudioEncoderInput(format: .f32, sampleRate: 48_000, channels: 1)
        let silence = AudioFrame(timestampUs: 0, data: Data(count: 960 * 4))

        // Release the codec before encoding: `output` retains it.
        do {
            let broadcast = try BroadcastProducer()
            var output: AudioEncoderOutput!
            do {
                let codec = AudioCodec.opus()
                output = AudioEncoderOutput(codec: codec)
            }
            let producer = try broadcast.encodeAudio(name: "mic", input: input, output: output)
            try producer.write(silence)
            XCTAssertEqual(try producer.name, "mic")
            try producer.finish()
            try broadcast.finish()
        }

        // Release the config before finishing: the producer retains what it needs.
        do {
            let broadcast = try BroadcastProducer()
            let producer: AudioProducer
            do {
                let codec = AudioCodec.opus()
                let output = AudioEncoderOutput(codec: codec)
                producer = try broadcast.encodeAudio(name: "mic", input: input, output: output)
            }
            try producer.write(silence)
            try producer.finish()
            try broadcast.finish()
        }
    }
}

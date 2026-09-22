import Foundation
import XCTest
@testable import Moq

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

    func testOriginProducerIsConstructible() throws {
        let origin = OriginProducer(cacheCapacityBytes: 4096)
        _ = origin.consume()
        _ = try origin.dynamic(prefix: "")
    }

    func testAnnounceThenUnannounceIsVisible() async throws {
        let origin = OriginProducer()
        let broadcast = try origin.createBroadcast(path: "live")
        _ = try broadcast.publishTrack(name: "events")
        try broadcast.announce()

        let announced = try origin.consume().announced(prefix: "")
        let first = try await announced.next()
        XCTAssertEqual(first?.prefix, "live")
        XCTAssertEqual(first?.active, true)

        try broadcast.unannounce()
        let retracted = try await announced.next()
        XCTAssertEqual(retracted?.prefix, "live")
        XCTAssertEqual(retracted?.active, false)
        _ = try await origin.consume().requestBroadcast(path: "live")
    }

    func testAnnouncedPatternCaptures() async throws {
        let origin = OriginProducer()
        let announced = try origin.consume().announced(prefix: "room", filter: "*/chat")
        let chat = try origin.createBroadcast(path: "room/alice/chat")
        try chat.announce()

        let update = try await announced.next()
        XCTAssertEqual(update?.prefix, "room/alice/chat")
        XCTAssertEqual(update?.captures, ["alice"])
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

    /// The decode side picks its CPU layout: an unset `format` is I420, and RGBA
    /// is four bytes a pixel, with each frame naming the layout it decoded to.
    func testDecodeVideoFormat() async throws {
        let origin = OriginProducer()
        let broadcast = try origin.createBroadcast(path: "video-decode-format")
        let video = try broadcast.encodeVideo(
            input: VideoEncoderInput(format: .rgba, width: 320, height: 240, framerate: 30),
            // Software both ways so the test is deterministic everywhere.
            output: VideoEncoderOutput(codec: .h264, track: "camera", kind: .software)
        )
        try broadcast.announce()

        // Seed the track so a subscriber joining below lands on encoded media.
        let rgba = Data(repeating: 0x80, count: 320 * 240 * 4)
        try video.cut()
        for i in 0..<10 {
            try video.write(VideoFrame(timestampUs: UInt64(i) * 33_333, data: rgba))
        }

        let consumer = try await origin.consume().requestBroadcast(path: "video-decode-format")
        let catalogs = try await consumer.subscribeCatalog()
        // XCTUnwrap takes an autoclosure, which can't hold an await.
        let nextCatalog = try await catalogs.next()
        let catalog = try XCTUnwrap(nextCatalog)
        let rendition = try XCTUnwrap(catalog.video["camera"])

        // Two subscribers over one publication, so the same encoded frames are
        // read twice and only the requested layout differs.
        let i420 = try await consumer.decodeVideo(name: "camera", catalogVideo: rendition)
        defer { i420.cancel() }
        let packed = try await consumer.decodeVideo(
            name: "camera",
            catalogVideo: rendition,
            output: VideoDecoderOutput(format: .rgba)
        )
        defer { packed.cancel() }

        // Keep the encoder fed so both decoders see frames after they joined.
        for i in 10..<40 {
            try video.write(VideoFrame(timestampUs: UInt64(i) * 33_333, data: rgba))
        }

        let nextPlanar = try await i420.next()
        let planar = try XCTUnwrap(nextPlanar)
        XCTAssertEqual(planar.format, .i420)
        XCTAssertEqual(planar.data.count, Int(planar.width) * Int(planar.height) * 3 / 2)

        let nextPacked = try await packed.next()
        let frame = try XCTUnwrap(nextPacked)
        XCTAssertEqual(frame.format, .rgba)
        XCTAssertEqual(frame.data.count, Int(frame.width) * Int(frame.height) * 4)
        XCTAssertTrue(stride(from: 3, to: frame.data.count, by: 4).allSatisfy { frame.data[$0] == 0xFF })

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

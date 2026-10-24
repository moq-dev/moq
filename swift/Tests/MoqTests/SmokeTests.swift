import Foundation
import XCTest
@testable import Moq

/// The next announce event.
private func nextRoute(_ announced: AnnounceConsumer) async throws -> AnnounceEvent {
    let next = try await announced.next()
    return try XCTUnwrap(next, "announce stream ended")
}

final class SmokeTests: XCTestCase {
    func testContainerTimebaseBelongsToOneBroadcast() throws {
        let owner = try BroadcastProducer()
        let other = try BroadcastProducer()
        let timebase = try Media.Timebase(broadcast: owner)
        XCTAssertThrowsError(try Media.ContainerProducer(broadcast: other, initData: Media.ContainerInit(format: .fmp4, data: Data(), timebase: timebase)))
        try owner.close()
        XCTAssertThrowsError(try Media.Timebase(broadcast: owner))
        try other.close()
    }

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

    func testAscendingSubscriptionReadsACachedBacklogOldestFirst() async throws {
        let broadcast = try BroadcastProducer()
        let track = try broadcast.publishTrack(name: "backlog", info: TrackInfo(timescale: 1_000_000))
        for ms: UInt64 in 0..<3 {
            let group = try track.appendGroup()
            try group.writeFrame(Data("x".utf8), timestampUs: ms * 1_000)
            try group.finish()
        }
        let consumer = try track.consume(subscription: Subscription(
            maxDelayUs: 10_000_000,
            ranges: [Subscription.Range(start: .relative(groups: 2))],
            order: .ascending
        ))
        var sequences: [UInt64] = []
        for _ in 0..<3 {
            let group = try await consumer.recvGroup()
            sequences.append(try XCTUnwrap(group).sequence)
        }
        XCTAssertEqual(sequences, [0, 1, 2])
    }

    /// An error's description is the Rust error message.
    func testErrorDescriptionIsRustMessage() {
        XCTAssertEqual(MoqError.Closed.description, "closed")
        XCTAssertEqual(MoqError.Transport(reason: "reset").description, "transport: reset")
    }

    /// Verifies the native lib loads and the wrapper compiles against the
    /// generated API. No network needed: we just instantiate a few types and
    /// exercise the cancel path.
    func testClientConstructsAndCancels() async throws {
        let client = try Client(tls: ClientTls(roots: [], systemRoots: true, fingerprints: []))
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

    /// A value the native side cannot use throws from the constructor, not the dial.
    func testInvalidConfigThrowsConfig() {
        XCTAssertThrowsError(try Client(versions: ["moq-lite-99"])) { error in
            guard case MoqError.Config = error else {
                return XCTFail("expected Config, got \(error)")
            }
        }
        XCTAssertThrowsError(try Server(bind: "not-an-address")) { error in
            guard case MoqError.Config = error else {
                return XCTFail("expected Config, got \(error)")
            }
        }
    }

    /// Both sides read the grant their peer gave them, and a peer that verifies no
    /// tokens of its own refuses another one as unsupported.
    func testSessionAuthReadsTheGrant() async throws {
        let version = ["moq-lite-07-wip"]
        let server = try Server(bind: "127.0.0.1:0", versions: version, tls: ServerTls(generate: ["localhost"]))
        defer { server.cancel() }
        let addr = try await server.listen()
        async let accepted = server.accept()

        let client = try Client(bind: "127.0.0.1:0", versions: version, tls: ClientTls(insecure: true), once: true)
        defer { client.cancel() }
        let session = try await client.connect(to: "https://\(addr)")
        guard let request = try await accepted else { return XCTFail("server closed before accept") }
        let peer = try await request.accept()

        // Ending both sessions ends any wait below, so a grant that never arrives fails
        // rather than hangs.
        let watchdog = Task {
            try await Task.sleep(nanoseconds: 10_000_000_000)
            peer.cancel(code: 0)
            session.cancel(code: 0)
        }
        defer { watchdog.cancel() }

        let everything = Grant(publish: ["**"], subscribe: ["**"], expiresUs: nil)
        for auth in [session.auth(), peer.auth()] {
            var grant = auth.grant()
            while grant != everything {
                grant = try await auth.grantChanged()
            }
        }

        do {
            _ = try await session.auth().add(token: "extra")
            XCTFail("the default acceptor refuses a token")
        } catch MoqError.Unsupported {}
        XCTAssertEqual(session.auth().grant(), everything)

        peer.cancel(code: 0)
        session.cancel(code: 0)
    }

    /// `cancel()` releases the listening socket before it returns, so the same
    /// address binds again with no retry.
    func testServerCloseReleasesPort() async throws {
        let first = try Server(bind: "127.0.0.1:0", tls: ServerTls(generate: ["localhost"]))
        let addr = try await first.listen()
        first.cancel()

        // No retry: cancel() closed the socket, so this binds on the first try.
        let second = try Server(bind: addr, tls: ServerTls(generate: ["localhost"]))
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
        guard case .start(let first) = try await nextRoute(announced) else {
            return XCTFail("expected an announcement")
        }
        XCTAssertEqual(first.prefix, "live")

        try broadcast.unannounce()
        guard case .end(let retracted) = try await nextRoute(announced) else {
            return XCTFail("expected a retraction")
        }
        XCTAssertEqual(retracted.prefix, "live")
        do {
            _ = try await consumer.requestBroadcast(path: "live")
            XCTFail("an unannounced broadcast must be unroutable")
        } catch {}

        try broadcast.announce()
        guard case .start = try await nextRoute(announced) else {
            return XCTFail("expected a reannouncement")
        }
        _ = try await consumer.requestBroadcast(path: "live")
    }

    func testAnnouncedPatternCaptures() async throws {
        let origin = OriginProducer()
        let announced = try origin.consume().announced(prefix: "room", filter: "*/chat")
        let chat = try origin.createBroadcast(path: "room/alice/chat")
        try chat.announce()

        guard case .start(let update) = try await nextRoute(announced) else {
            return XCTFail("expected an announcement")
        }
        XCTAssertEqual(update.prefix, "room/alice/chat")
        XCTAssertEqual(update.captures, ["alice"])
    }

    func testAnnouncedEpochRestarts() async throws {
        let origin = OriginProducer()
        let announced = try origin.consume().announced(prefix: "")
        let broadcast = try origin.createBroadcast(path: "epoch")

        let first = mintEpoch()
        XCTAssertEqual(try epochTime("018bcfe5-6800-7000-8000-000000000000"), Date(timeIntervalSince1970: 1_700_000_000))
        try broadcast.announce(route: AnnounceRoute(epoch: first))
        guard case .start(let start) = try await nextRoute(announced) else {
            return XCTFail("expected an announcement")
        }
        XCTAssertEqual(start.route.epoch, first)

        let second = mintEpoch()
        try broadcast.announce(route: AnnounceRoute(epoch: second))
        guard case .restart(let restart) = try await nextRoute(announced) else {
            return XCTFail("expected a restart")
        }
        XCTAssertEqual(restart.route.epoch, second)

        XCTAssertThrowsError(try epochTime("not-an-epoch"))
    }

    func testDynamicServesARequestUnderAPrefix() async throws {
        let origin = OriginProducer()
        let dynamic = try origin.dynamic(prefix: "live")
        let pending = Task {
            try await origin.consume().requestBroadcast(path: "live/cam")
        }
        let request = try await dynamic.requestedBroadcast()
        XCTAssertEqual(try request.path, "live/cam")
        let served = try request.prepare(epoch: mintEpoch()).publish()
        _ = try await pending.value
        try served.close()
        dynamic.cancel()
    }


    func testBroadcastProducerOpensTracks() throws {
        let broadcast = try BroadcastProducer()
        let track = try broadcast.publishTrack(name: "events")
        XCTAssertEqual(try track.demand().name, "events")
        try track.finish()
        try broadcast.close()
    }

    func testBroadcastCloseTwiceIsNoop() throws {
        let broadcast = try BroadcastProducer()
        try broadcast.close()
        try broadcast.close()
        XCTAssertThrowsError(try broadcast.publishTrack(name: "events"))
    }

    func testFinishedBroadcastLingersUntilTheWaitIsCancelled() async throws {
        let broadcast = try BroadcastProducer()
        let track = try broadcast.publishTrack(name: "status", info: TrackInfo(maxAgeUs: 3_600_000_000))
        try track.finish()
        let finish = try broadcast.finish()
        XCTAssertEqual(finish.lingerUs, 3_600_000_000)
        XCTAssertThrowsError(try broadcast.publishTrack(name: "events"))
        // Cancelling the wait is the interrupt: it ends instead of lingering for an hour.
        let wait = Task { try await finish.wait() }
        await Task.yield()
        wait.cancel()
        _ = await wait.result
        finish.close()
    }

    func testVideoHintsReachMediaPublishApi() throws {
        let broadcast = try BroadcastProducer()
        let hint = Media.VideoHint(
            coded: Media.Dimensions(width: 1920, height: 1080),
            bitrate: 4_000_000,
            framerate: 60,
            optimizeForLatency: true
        )
        let media = try Media.TrackProducer.video(broadcast: broadcast, initData: Media.VideoInit(format: .avc3, data: Data(), hint: hint))
        try media.finish()
        try broadcast.close()
    }

    func testVideoPropertiesUseDefaultedFields() throws {
        let broadcast = try BroadcastProducer()
        try Media.CatalogProducer(broadcast: broadcast).setVideoProperties(Media.VideoProperties(rotation: 315))
        try broadcast.close()
    }

    func testCatalogHandleClosesWithBroadcast() throws {
        let broadcast = try BroadcastProducer()
        let catalog = try Media.CatalogProducer(broadcast: broadcast)
        try catalog.setSection(name: "app", json: "{\"value\":42}")
        _ = try catalog.now()
        try broadcast.close()
        XCTAssertThrowsError(try catalog.removeSection(name: "app"))
        XCTAssertThrowsError(try catalog.now())
    }

    func testBroadcastConsumerFetchesCachedGroup() async throws {
        let broadcast = try BroadcastProducer()
        let track = try broadcast.publishTrack(name: "events", info: TrackInfo(timescale: 1_000_000))
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
        let producer = try Json.SnapshotProducer<Status>(
            broadcast: broadcast,
            track: try broadcast.publishTrack(name: "status", info: TrackInfo(timescale: 1_000_000)),
            compression: true)
        let consumer = try Json.SnapshotConsumer<Status>(
            track: try await broadcast.consume().subscribeTrack(name: "status"), compression: true)

        for viewers in 1...4 {
            try producer.update(Status(state: "live", viewers: viewers), timestampUs: UInt64(viewers) * 1000)
        }

        // `next` yields every state in order; `latest` skips the buffered third.
        let first = try await consumer.next()
        XCTAssertEqual(first, Json.Timed(value: Status(state: "live", viewers: 1), timestampUs: 1000))
        let second = try await consumer.next()
        XCTAssertEqual(second, Json.Timed(value: Status(state: "live", viewers: 2), timestampUs: 2000))
        let latest = try await consumer.latest()
        XCTAssertEqual(latest, Json.Timed(value: Status(state: "live", viewers: 4), timestampUs: 4000))

        consumer.cancel()
        try producer.finish()
        try broadcast.close()
    }

    func testJsonStreamRoundTrip() async throws {
        struct Event: Codable, Equatable {
            let n: Int
        }

        let broadcast = try BroadcastProducer()
        let producer = try Json.StreamProducer<Event>(
            broadcast: broadcast, track: try broadcast.publishTrack(name: "events"))
        let consumer = try Json.StreamConsumer<Event>(
            track: try await broadcast.consume().subscribeTrack(name: "events"))

        for n in 0..<3 {
            try producer.append(Event(n: n))
            let record = try await consumer.next()
            XCTAssertEqual(record, Json.Timed(value: Event(n: n), timestampUs: nil))
        }

        consumer.cancel()
        try producer.finish()
        try broadcast.close()
    }

    func testJsonProducersReportDemand() async throws {
        let broadcast = try BroadcastProducer()
        let snapshot = try Json.SnapshotProducer<[String: Int]>(
            broadcast: broadcast, track: try broadcast.publishTrack(name: "status"))
        let stream = try Json.StreamProducer<[String: Int]>(
            broadcast: broadcast, track: try broadcast.publishTrack(name: "events"))
        let snapshotDemand = try snapshot.demand()
        let streamDemand = try stream.demand()
        XCTAssertEqual(snapshotDemand.name, "status")
        XCTAssertNil(snapshotDemand.wanted)
        let consumer = try broadcast.consume()

        let snapshotConsumer = try Json.SnapshotConsumer<[String: Int]>(
            track: try await consumer.subscribeTrack(name: "status"))
        let streamConsumer = try Json.StreamConsumer<[String: Int]>(
            track: try await consumer.subscribeTrack(name: "events"))
        while try await snapshotDemand.wantedChanged() == nil {}
        while try await streamDemand.wantedChanged() == nil {}
        XCTAssertEqual(snapshotDemand.wanted, 0)

        snapshotConsumer.cancel()
        streamConsumer.cancel()
        while try await snapshotDemand.wantedChanged() != nil {}
        while try await streamDemand.wantedChanged() != nil {}

        try broadcast.close()
    }

    func testFlateTimestampsRoundTrip() async throws {
        for timestamp in [nil, UInt64(123_456)] {
            for mode in ["snapshot", "stream"] {
                let broadcast = try BroadcastProducer()
                let info: TrackInfo? = timestamp.map { _ in TrackInfo(timescale: 1_000_000) }
                let track = try broadcast.publishTrack(name: "data", info: info)
                let consumer = try track.consume()
                let payload = Data("payload".utf8)
                if mode == "snapshot" {
                    let producer = try Flate.SnapshotProducer(broadcast: broadcast, track: track)
                    try producer.update(payload, timestampUs: timestamp)
                    try producer.finish()
                } else {
                    let producer = try Flate.StreamProducer(broadcast: broadcast, track: track)
                    try producer.append(payload, timestampUs: timestamp)
                    try producer.finish()
                }
                let received = try await consumer.readFrame()
                let frame = try XCTUnwrap(received)
                XCTAssertEqual(frame.payload, payload)
                XCTAssertEqual(frame.timestampUs, timestamp)
                try broadcast.close()
            }
        }
    }

    func testRawTrackWithoutTimestampStaysUntimed() async throws {
        let broadcast = try BroadcastProducer()
        let track = try broadcast.publishTrack(name: "untimed")
        let consumer = try track.consume()
        try track.writeFrame(Data("payload".utf8))
        let received = try await consumer.readFrame()
        let frame = try XCTUnwrap(received)
        XCTAssertEqual(frame.payload, Data("payload".utf8))
        XCTAssertNil(frame.timestampUs)
        XCTAssertNil(try consumer.info().timescale)
        try track.finish()
        try broadcast.close()
    }

    func testRawTrackTimestamps() async throws {
        let broadcast = try BroadcastProducer()
        let track = try broadcast.publishTrack(name: "events", info: TrackInfo(timescale: 1_000_000))
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
        try broadcast.close()
    }

    func testReadFrameSkipsEmptyThenPopulatedGroups() async throws {
        let broadcast = try BroadcastProducer()
        let track = try broadcast.publishTrack(name: "status", info: TrackInfo(timescale: 1_000_000))
        let consumer = try track.consume()

        try track.appendGroup().finish()
        try track.appendGroup().finish()
        try track.writeFrame(Data("populated".utf8), timestampUs: 2_000)

        let frame = try await consumer.readFrame()
        XCTAssertEqual(frame?.payload, Data("populated".utf8))
        XCTAssertEqual(frame?.timestampUs, 2_000)

        try track.finish()
        try broadcast.close()
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
        try broadcast.close()
    }

    /// `frameDurationUs` is microseconds so Opus' 2.5 ms frame is expressible at
    /// all, and a duration outside the Opus set is refused rather than silently
    /// rounded.
    func testEncodeAudioFrameDurations() throws {
        let broadcast = try BroadcastProducer()
        let input = AudioEncoderInput(format: .f32, sampleRate: 48_000, channels: 1)

        let catalog = try Media.CatalogProducer(broadcast: broadcast)
        let fine = try Audio.Producer(
            catalog: catalog,
            input: input,
            output: AudioEncoderOutput(codec: AudioCodec.opus(),  frameDurationUs: 2_500),
            track: "fine"
        )
        // 2.5 ms of silence at 48 kHz mono f32: exactly one encoded frame.
        try fine.write(AudioFrame(timestampUs: 0, data: Data(count: 120 * 4)))
        try fine.finish()

        XCTAssertThrowsError(
            try Audio.Producer(
                catalog: catalog,
                input: input,
                output: AudioEncoderOutput(codec: AudioCodec.opus(),  frameDurationUs: 2_000),
                track: "coarse"
            )
        ) { error in
            if let audio = error as? MoqError, case .Audio = audio { return }
            XCTFail("2 ms is not an opus frame duration: \(error)")
        }

        try broadcast.close()
    }

    /// A fixed playout delay is reported back as is, and an estimated one moves
    /// as frames are read.
    func testAudioConsumerReportsItsPlayoutDelay() async throws {
        let origin = OriginProducer()
        let broadcast = try origin.createBroadcast(path: "audio-delay")
        let audio = try Audio.Producer(
            catalog: Media.CatalogProducer(broadcast: broadcast),
            input: AudioEncoderInput(format: .f32, sampleRate: 48_000, channels: 1),
            output: AudioEncoderOutput(codec: AudioCodec.opus(), ),
            track: "mic"
        )
        try broadcast.announce()

        let consumer = try await origin.consume().requestBroadcast(path: "audio-delay")
        let catalogs = try await Media.CatalogConsumer.subscribe(broadcast: consumer)
        // XCTUnwrap takes an autoclosure, which can't hold an await.
        let nextCatalog = try await catalogs.next()
        let catalog = try XCTUnwrap(nextCatalog)
        let rendition = try XCTUnwrap(catalog.audio["mic"])

        let fixed = try await Audio.Consumer.subscribe(
            broadcast: consumer, id: "mic", rendition: rendition,
            output: AudioDecoderOutput(format: .f32, delayUs: 80_000)
        )
        let estimated = try await Audio.Consumer.subscribe(
            broadcast: consumer, id: "mic", rendition: rendition,
            output: AudioDecoderOutput(format: .f32)
        )
        XCTAssertEqual(fixed.delayUs, 80_000)
        let initial = estimated.delayUs

        // Two 20 ms Opus frames of silence at 48 kHz mono.
        for i in 0..<2 {
            try audio.write(AudioFrame(timestampUs: UInt64(i) * 20_000, data: Data(count: 960 * 4)))
        }
        let fixedFrame = try await fixed.next()
        XCTAssertNotNil(fixedFrame)
        let estimatedFrame = try await estimated.next()
        XCTAssertNotNil(estimatedFrame)

        XCTAssertEqual(fixed.delayUs, 80_000)
        XCTAssertNotEqual(estimated.delayUs, initial, "the estimate should move as frames are read")

        fixed.cancel()
        estimated.cancel()
        try audio.finish()
        try broadcast.close()
    }

    /// A decoded frame owns its picture: it converts to either CPU layout on
    /// demand and stays readable after its consumer is cancelled.
    func testDecodeVideoFrame() async throws {
        let origin = OriginProducer()
        let broadcast = try origin.createBroadcast(path: "video-decode-frame")
        let video = try Video.Producer(
            catalog: Media.CatalogProducer(broadcast: broadcast),
            input: VideoEncoderInput(format: .rgba, width: 320, height: 240, framerate: Video.FrameRate(numerator: 30000, denominator: 1001)),
            output: VideoEncoderOutput(codec: .h264, kind: .software),
            track: "camera"
        )
        try broadcast.announce()

        // Seed the track so a subscriber joining below lands on encoded media.
        let rgba = Data(repeating: 0x80, count: 320 * 240 * 4)
        try video.cut()
        for i in 0..<10 {
            try video.write(VideoFrame(timestampUs: UInt64(i) * 33_333, data: rgba))
        }

        let consumer = try await origin.consume().requestBroadcast(path: "video-decode-frame")
        let catalogs = try await Media.CatalogConsumer.subscribe(broadcast: consumer)
        // XCTUnwrap takes an autoclosure, which can't hold an await.
        let nextCatalog = try await catalogs.next()
        let catalog = try XCTUnwrap(nextCatalog)
        let rendition = try XCTUnwrap(catalog.video["camera"])

        let decoder = try await Video.Consumer.subscribe(broadcast: consumer, id: "camera", rendition: rendition)

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
        try broadcast.close()
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
            let producer = try Audio.Producer(
                catalog: Media.CatalogProducer(broadcast: broadcast),
                input: input,
                output: output,
                track: "mic"
            )
            try producer.write(silence)
            XCTAssertEqual(try producer.demand().name, "mic")
            try producer.finish()
            try broadcast.close()
        }

        // Release the config before finishing: the producer retains what it needs.
        do {
            let broadcast = try BroadcastProducer()
            let producer: Audio.Producer
            do {
                let codec = AudioCodec.opus()
                let output = AudioEncoderOutput(codec: codec)
                producer = try Audio.Producer(
                    catalog: Media.CatalogProducer(broadcast: broadcast),
                    input: input,
                    output: output,
                    track: "mic"
                )
            }
            try producer.write(silence)
            try producer.finish()
            try broadcast.close()
        }
    }
}

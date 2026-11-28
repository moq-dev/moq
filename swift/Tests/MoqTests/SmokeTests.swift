import Foundation
import XCTest

@testable import Moq

/// The next announce event.
private func nextRoute(_ announced: AnnounceConsumer) async throws -> AnnounceEvent {
    let next = try await announced.next()
    return try XCTUnwrap(next, "announce stream ended")
}

/// A timestamp in microseconds.
private func micros(_ value: UInt64) -> Timestamp {
    Timestamp(value: value, scale: 1_000_000)
}

/// Assert `body` throws `MoqError.TimeOverflow`.
private func assertTimeOverflow<T>(
    _ body: @autoclosure () throws -> T, file: StaticString = #filePath, line: UInt = #line
) {
    XCTAssertThrowsError(try body(), file: file, line: line) { error in
        guard case MoqError.TimeOverflow = error else {
            return XCTFail("expected TimeOverflow, got \(error)", file: file, line: line)
        }
    }
}

final class SmokeTests: XCTestCase {
    func testDatagramOnlyRequestsHaveIndependentPriorityDemand() async throws {
        let broadcast = try BroadcastProducer()
        let stream = try broadcast.publishTrack(name: "events")
        let requests = try broadcast.requests().datagrams()
        let consumer = try broadcast.consume().datagrams(name: "events")
        try consumer.setPriority(12)
        let request = try await requests.requested()
        XCTAssertEqual(try request.name(), "events")
        XCTAssertEqual(try request.priority(), 12)
        let producer = try request.accept()
        XCTAssertEqual(try producer.priority(), 12)
        XCTAssertFalse(try stream.demand().wanted)
        try producer.insert(Datagram(sequence: 7, timestamp: .unscaled(7), payload: Data("seven".utf8)))
        let datagram = try await consumer.recv()
        XCTAssertEqual(datagram?.sequence, 7)
        XCTAssertEqual(datagram?.payload, Data("seven".utf8))
        // A scale-less datagram's unscaled timestamp names the sequence it writes at.
        XCTAssertEqual(try producer.append(Data("nine".utf8), timestamp: .unscaled(9)), 9)
        let nine = try await consumer.recv()
        XCTAssertEqual(nine?.timestamp, .unscaled(9))
        consumer.cancel()
        let priority = try await producer.priorityChanged(last: 12)
        XCTAssertNil(priority)
        try producer.finish()
        requests.cancel()
        try broadcast.close()
    }

    func testDefaultSubscriptionSkipsSupersededGroupsAtOnce() {
        XCTAssertNil(Subscription.default.delay.min)
        XCTAssertEqual(Subscription.default.delay.max, 0)
    }

    func testContainerTimebaseBelongsToOneBroadcast() throws {
        let owner = try BroadcastProducer()
        let other = try BroadcastProducer()
        let timebase = try Media.Timebase(broadcast: owner)
        XCTAssertThrowsError(
            try Media.ContainerProducer(
                broadcast: other, initData: Media.ContainerInit(format: .fmp4(), data: Data(), timebase: timebase)))
        try owner.close()
        XCTAssertThrowsError(try Media.Timebase(broadcast: owner))
        try other.close()
    }

    func testStreamAbortPreservesProtocolDetails() async throws {
        let broadcast = try BroadcastProducer()
        let track = try broadcast.publishTrack(name: "errors")
        let producer = try track.appendGroup(start: .unscaled(0))
        let consumer = try broadcast.consume()
        let group = try await consumer.fetchGroup(name: "errors", start: 0)
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
        let info = TrackInfo(maxAge: nil, timescale: 1_000_000)
        let track = try broadcast.publishTrack(name: "backlog", info: info)
        for ms: UInt64 in 0..<3 {
            let group = try track.appendGroup(start: micros(ms * 1_000))
            try group.writeFrame(Data("x".utf8), timestamp: micros(ms * 1_000))
            try group.finish()
        }
        let consumer = try track.consume(
            subscription: Subscription(
                delay: Delay(min: 0, max: 10),
                ranges: [Subscription.Range(start: .unscaled(0), order: .ascending)]
            ))
        var starts: [UInt64] = []
        for _ in 0..<3 {
            let group = try await consumer.recvGroup()
            starts.append(try XCTUnwrap(group).start)
        }
        XCTAssertEqual(starts, [0, 1_000, 2_000])
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
                error.isShutdown
                    || {
                        if case .Connect = error { return true } else { return false }
                    }()
                    || {
                        if case .Url = error { return true } else { return false }
                    }(),
                "expected shutdown/connect/url error, got: \(error)"
            )
        }
    }

    /// A viewer on a session that has not connected reads no estimate.
    func testViewerHoldsWithoutAnEstimate() async throws {
        let client = try Client(bind: "127.0.0.1:0", tls: ClientTls(insecure: true), once: true)
        defer { client.cancel() }
        let session = try await client.connect(to: "https://127.0.0.1:1")
        defer { session.cancel(code: 0) }

        let viewers = session.viewers()
        let viewer = try viewers.watch(maxBps: 1_000_000)
        try viewer.update(maxBps: 2_000_000)
        try viewers.setTarget(targetBps: 0)
        try viewers.setTarget(targetBps: nil)
        XCTAssertNil(viewer.rate())
        // Past what a PROBE target carries: refused.
        XCTAssertThrowsError(try viewers.watch(maxBps: 1 << 53)) { error in
            guard case MoqError.OutOfRange = error else {
                return XCTFail("expected OutOfRange, got \(error)")
            }
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

    /// A negative, infinite, or NaN duration throws `Config`, where the generated converter would abort.
    func testBadDurationThrowsConfig() async throws {
        func assertConfig(_ body: () throws -> Void, _ what: String) {
            XCTAssertThrowsError(try body(), what) { error in
                guard case MoqError.Config = error else {
                    return XCTFail("\(what): expected Config, got \(error)")
                }
            }
        }
        for bad in [-1, Double.infinity, Double.nan] {
            assertConfig({ _ = try Client(timeout: bad) }, "client timeout \(bad)")
            assertConfig({ _ = try Client(quic: QuicConfig(idleTimeout: bad)) }, "quic idle \(bad)")
            assertConfig({ _ = try Client(backoff: Backoff(initial: bad)) }, "backoff initial \(bad)")
            assertConfig({ _ = try Server(quic: QuicConfig(keepAlive: bad)) }, "server keep-alive \(bad)")

            let broadcast = try BroadcastProducer()
            var info = try broadcast.publishTrack(name: "info").consume().info()
            info.maxAge = bad
            assertConfig({ _ = try broadcast.publishTrack(name: "bad", info: info) }, "max age \(bad)")
            info.maxAge = 0
            info.refresh = TrackRefresh(interval: bad)
            assertConfig({ _ = try broadcast.publishTrack(name: "bad", info: info) }, "refresh interval \(bad)")

            var subscription = Subscription.default
            subscription.delay.max = bad
            let track = try broadcast.publishTrack(name: "events")
            assertConfig({ _ = try track.consume(subscription: subscription) }, "delay max \(bad)")
            assertConfig({ try track.consume().update(subscription: subscription) }, "update delay max \(bad)")
            do {
                _ = try await broadcast.consume().subscribeTrack(name: "events", subscription: subscription)
                XCTFail("subscribeTrack accepted delay \(bad)")
            } catch MoqError.Config {}
            do {
                _ = try await Media.CatalogConsumer.subscribe(broadcast: broadcast.consume(), delay: bad)
                XCTFail("catalog subscribe accepted delay \(bad)")
            } catch MoqError.Config {}
            assertConfig(
                {
                    _ = try Video.Encoder(
                        input: VideoEncoderInput(
                            format: .rgba(), width: 64, height: 64,
                            framerate: Video.FrameRate(numerator: 30, denominator: 1)),
                        output: VideoEncoderOutput(codec: VideoCodec.h264(), join: bad, kind: .software))
                }, "video join \(bad)")
            assertConfig(
                { _ = try Audio.Encoder(output: AudioEncoderOutput(codec: AudioCodec.opus(), frameDuration: bad)) },
                "audio frame duration \(bad)")
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
        // The wrapper names itself ahead of moq-ffi, so a server can tell bindings apart.
        XCTAssertTrue(
            request.peerImplementation?.hasPrefix("moq-swift/") == true,
            "\(String(describing: request.peerImplementation))")
        let peer = try await request.accept()

        // Ending both sessions ends any wait below, so a grant that never arrives fails
        // rather than hangs.
        let watchdog = Task {
            try await Task.sleep(nanoseconds: 10_000_000_000)
            peer.cancel(code: 0)
            session.cancel(code: 0)
        }
        defer { watchdog.cancel() }

        let everything = Grant(publish: [""], subscribe: [""], expires: nil)
        for side in [session, peer] {
            for try await grant in side.grants() where grant == everything {
                break
            }
        }
        XCTAssertNil(peer.tokens(), "an accepted session presents no tokens")

        guard let tokens = session.tokens() else { return XCTFail("a client session has tokens") }
        try tokens.set(["extra"])
        do {
            try await tokens.answered()
            XCTFail("the default acceptor refuses a token")
        } catch MoqError.Unsupported {}
        XCTAssertEqual(session.grants().grant(), everything)

        peer.cancel(code: 0)
        session.cancel(code: 0)
    }

    /// A credential is answered by the peer, names a route and a claim, and cancels.
    func testSessionCredentialLifecycle() async throws {
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

        XCTAssertThrowsError(try session.credential("")) { error in
            guard case MoqError.Config = error else {
                return XCTFail("expected Config, got \(error)")
            }
        }
        let credential = try session.credential("extra")
        do {
            try await credential.answered()
            XCTFail("the default acceptor refuses a token")
        } catch MoqError.Unsupported {}

        let route = try session.publish.route(path: "room/x", hidden: true)
        let claim = try session.publish.claim(prefix: "live", hidden: true)
        for target in [route.setAuth, claim.setAuth] {
            try target(credential)
            try target(nil)
        }

        credential.cancel()
        do {
            try await credential.answered()
            XCTFail("a cancelled credential has no answer")
        } catch MoqError.Cancelled {}
        for target in [route.setAuth, claim.setAuth] {
            XCTAssertThrowsError(try target(credential)) { error in
                guard case MoqError.Cancelled = error else {
                    return XCTFail("expected Cancelled, got \(error)")
                }
            }
        }
        route.cancel()
        claim.cancel()

        peer.cancel(code: 0)
        session.cancel(code: 0)
    }

    /// Over moq-lite-06, whose wire carries no identity, the session mints one: passing it back
    /// pins `request` and `follow`, while its text parsed is another, named identity that nothing
    /// serves.
    func testAnnouncedIdentityPinsRequestAndFollow() async throws {
        let version = ["moq-lite-06"]
        let published = OriginProducer()
        let route = try published.route(path: "live")
        defer { route.cancel() }
        let server = try Server(
            bind: "127.0.0.1:0", versions: version, tls: ServerTls(generate: ["localhost"]), publish: published)
        defer { server.cancel() }
        let addr = try await server.listen()
        async let accepted = server.accept()

        let viewer = OriginProducer()
        let client = try Client(
            bind: "127.0.0.1:0", versions: version, tls: ClientTls(insecure: true), once: true, consume: viewer)
        defer { client.cancel() }
        let session = try await client.connect(to: "https://\(addr)")
        guard let request = try await accepted else { return XCTFail("server closed before accept") }
        let peer = try await request.accept()
        defer {
            peer.cancel(code: 0)
            session.cancel(code: 0)
        }

        let consumer = viewer.consume()
        guard case .present(let present) = try await nextRoute(try consumer.announced()) else {
            return XCTFail("expected an announcement")
        }
        let minted = present.route.identity
        XCTAssertTrue(minted.isMinted())

        _ = try await consumer.request(path: "live", identity: minted)
        let following = try consumer.follow(path: "live", identity: minted)
        // Cancelling ends the wait below, so an identity that never resolves fails rather than hangs.
        let watchdog = Task {
            try await Task.sleep(nanoseconds: 10_000_000_000)
            following.cancel()
        }
        defer { watchdog.cancel() }
        let followed = try await following.available()
        XCTAssertNotNil(followed)
        following.cancel()

        let named = try Identity.parse(text: minted.text())
        XCTAssertFalse(named.isMinted())
        XCTAssertEqual(named.description, minted.text())
        XCTAssertNotEqual(named, minted)
        do {
            _ = try await consumer.request(path: "live", identity: named)
            XCTFail("the same text, named, is another identity")
        } catch {}
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
        let claim = try origin.claim(prefix: "")
        claim.cancel()
    }

    func testBroadcastIsReachableOnlyWhileAnnounced() async throws {
        let origin = OriginProducer()
        let route = try origin.route(path: "live", hidden: true)
        _ = try route.broadcast.publishTrack(name: "events")
        let consumer = origin.consume()
        do {
            _ = try await consumer.request(path: "live")
            XCTFail("a hidden route must be unroutable")
        } catch {}

        try route.announce()
        let announced = try consumer.announced(prefix: "")
        guard case .present(let first) = try await nextRoute(announced) else {
            return XCTFail("expected an announcement")
        }
        XCTAssertEqual(first.prefix, "live")
        XCTAssertEqual(first.route.identity, route.identity)

        try route.withdraw()
        guard case .absent(let retracted) = try await nextRoute(announced) else {
            return XCTFail("expected a retraction")
        }
        XCTAssertEqual(retracted.prefix, "live")
        do {
            _ = try await consumer.request(path: "live")
            XCTFail("a withdrawn route must be unroutable")
        } catch {}

        // Announcing again continues the same stream.
        try route.announce()
        guard case .present(let again) = try await nextRoute(announced) else {
            return XCTFail("expected a reannouncement")
        }
        XCTAssertEqual(again.route.identity, route.identity)
        _ = try await consumer.request(path: "live")
        route.cancel()
    }

    /// Cancelling a route retracts it while its broadcast lives on; closing the broadcast
    /// ends the route too.
    func testCancellingARouteRetractsIt() async throws {
        let origin = OriginProducer()
        let consumer = origin.consume()
        let route = try origin.route(path: "live")
        let broadcast = route.broadcast
        XCTAssertTrue(broadcast === route.broadcast)
        _ = try await consumer.request(path: "live")

        route.cancel()
        do {
            _ = try await consumer.request(path: "live")
            XCTFail("a cancelled route must be unroutable")
        } catch {}
        _ = try broadcast.publishTrack(name: "events")
        try broadcast.close()

        let ended = try origin.route(path: "ended")
        try ended.broadcast.close()
        XCTAssertThrowsError(try ended.announce()) { error in
            guard case MoqError.Closed = error else {
                return XCTFail("expected Closed, got \(error)")
            }
        }
    }

    /// A route outliving its closed broadcast cannot touch the one replacing it.
    func testStaleRouteLeavesItsReplacementAlone() async throws {
        let origin = OriginProducer()
        let stale = try origin.route(path: "live")
        try stale.broadcast.close()

        let replacement = try origin.route(path: "live")
        XCTAssertThrowsError(try stale.announce()) { error in
            guard case MoqError.Closed = error else {
                return XCTFail("expected Closed, got \(error)")
            }
        }
        XCTAssertThrowsError(try stale.setCost(1)) { error in
            guard case MoqError.Closed = error else {
                return XCTFail("expected Closed, got \(error)")
            }
        }
        try stale.withdraw()
        _ = try await origin.consume().request(path: "live")
        try replacement.broadcast.close()
    }

    func testAnnouncedListsAPrefixAndRefusesAWildcard() async throws {
        let origin = OriginProducer()
        let announced = try origin.consume().announced(prefix: "room/alice")
        let other = try origin.route(path: "room/bob/chat")
        let chat = try origin.route(path: "room/alice/chat")

        guard case .present(let update) = try await nextRoute(announced) else {
            return XCTFail("expected an announcement")
        }
        XCTAssertEqual(update.prefix, "room/alice/chat")
        XCTAssertThrowsError(try origin.consume().announced(prefix: "room/*")) { error in
            guard case MoqError.InvalidPrefix = error else {
                return XCTFail("expected InvalidPrefix, got \(error)")
            }
        }
        other.cancel()
        chat.cancel()
    }

    func testNewIdentityRestartsFollowersButStaysListed() async throws {
        let origin = OriginProducer()
        let consumer = origin.consume()
        let announced = try consumer.announced(prefix: "")
        let following = try consumer.follow(path: "identity")
        let first = try origin.route(path: "identity")
        guard case .present(let present) = try await nextRoute(announced) else {
            return XCTFail("expected an announcement")
        }
        XCTAssertFalse(present.route.identity.isMinted())
        XCTAssertEqual(present.route.identity, first.identity)
        let firstBroadcast = try await following.available()
        XCTAssertNotNil(firstBroadcast)

        let camera = try Identity.parse(text: "camera")
        let second = try origin.route(path: "identity", identity: camera, epoch: 1)
        let takeover = try await following.available()
        XCTAssertNotNil(takeover)

        // The takeover was not listed, so the next event is the path going absent.
        try first.withdraw()
        try second.withdraw()
        guard case .absent(let absent) = try await nextRoute(announced) else {
            return XCTFail("expected the path to go absent")
        }
        XCTAssertEqual(absent.route.identity, camera)
        XCTAssertEqual(absent.route.identity.description, "camera")

        XCTAssertThrowsError(try Identity.parse(text: "")) { error in
            guard case MoqError.InvalidIdentity = error else {
                return XCTFail("expected InvalidIdentity, got \(error)")
            }
        }
        first.cancel()
        second.cancel()
        following.cancel()
    }

    func testFollowerReportsOfflineAndTheNextBroadcast() async throws {
        let origin = OriginProducer()
        let following = try origin.consume().follow(path: "offline")
        let route = try origin.route(path: "offline")

        let first = try await following.available()
        XCTAssertNotNil(first)

        try route.withdraw()
        let offline = try await following.available()
        XCTAssertNil(offline)

        try route.announce()
        let returned = try await following.available()
        XCTAssertNotNil(returned)
        route.cancel()
        following.cancel()
    }

    func testDynamicServesARequestUnderAPrefix() async throws {
        let origin = OriginProducer()
        let dynamic = try origin.claim(prefix: "live")
        let pending = Task {
            try await origin.consume().request(path: "live/cam")
        }
        let requested = try await dynamic.requested()
        let request = try XCTUnwrap(requested)
        XCTAssertEqual(try request.path, "live/cam")
        let served = try request.route(hidden: true)
        XCTAssertEqual(served.identity, dynamic.identity)
        let broadcast = served.broadcast
        XCTAssertTrue(broadcast === served.broadcast)
        // Only announcing answers the requester.
        try served.announce()
        _ = try await pending.value
        try broadcast.close()
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

    func testFinishedCatalogLeavesTheBroadcastUpUntilClosed() throws {
        let broadcast = try BroadcastProducer()
        let catalog = try Media.CatalogProducer(broadcast: broadcast)
        let track = try broadcast.publishTrack(name: "status")
        try track.finish()
        try catalog.finish()
        // Finishing the content leaves the broadcast up until it closes.
        _ = try broadcast.consume()
        try broadcast.close()
        XCTAssertThrowsError(try catalog.finish())
    }

    func testBroadcastConsumerClosedReturnsOnClose() async throws {
        let broadcast = try BroadcastProducer()
        let consumer = try broadcast.consume()
        let closed = Task { try await consumer.closed() }
        try broadcast.close()
        try await closed.value
    }

    func testVideoHintsReachMediaPublishApi() throws {
        let broadcast = try BroadcastProducer()
        let hint = Media.VideoHint(
            coded: Media.Dimensions(width: 1920, height: 1080),
            bitrate: 4_000_000,
            framerate: 60,
            optimizeForLatency: true
        )
        let media = try Media.TrackProducer.video(
            broadcast: broadcast, initData: Media.VideoInit(format: .h264(), data: Data(), hint: hint))
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

    /// Updates published before the reader joins reach a holding reader in order, each with its
    /// timestamp; an arrival reader takes only the newest.
    func testHoldingCatalogReaderReadsEveryUpdateWithinItsDelay() async throws {
        let broadcast = try BroadcastProducer()
        defer { try? broadcast.close() }
        let catalog = try Media.CatalogProducer(broadcast: broadcast)
        for value in ["1", "2"] {
            try catalog.setSection(name: "app", json: value)
        }
        let consumer = try broadcast.consume()

        for (delay, expected) in [(0.0, ["2"]), (60.0, ["1", "2"])] {
            let reader = try await Media.CatalogConsumer.subscribe(broadcast: consumer, delay: delay)
            var read: [String] = []
            var last: UInt64 = 0
            while read.last != "2" {
                let next = try await reader.next()
                let update = try XCTUnwrap(next, "\(delay)s delay")
                XCTAssertNotNil(update.timestamp.scale)
                XCTAssertGreaterThan(update.timestamp.value, last, "\(delay)s delay")
                last = update.timestamp.value
                // The broadcast's first catalog, published before any section, is not one of ours.
                if let app = update.sections["app"] {
                    read.append(app)
                }
            }
            reader.cancel()
            XCTAssertEqual(read, expected, "\(delay)s delay")
        }
    }

    func testBroadcastConsumerFetchesCachedGroup() async throws {
        let broadcast = try BroadcastProducer()
        let track = try broadcast.publishTrack(name: "events", info: TrackInfo(maxAge: 0, timescale: 1_000_000))
        let group = try track.appendGroup(start: micros(0))
        try group.writeFrame(Data("cached".utf8), timestamp: micros(0))
        try group.finish()

        let consumer = try broadcast.consume()
        let fetched = try await consumer.fetchGroup(
            name: "events",
            start: 0,
            options: FetchGroupOptions(priority: 3)
        )
        XCTAssertEqual(fetched.start, 0)
        let frame = try await fetched.readFrame()
        XCTAssertEqual(frame?.payload, Data("cached".utf8))
        let end = try await fetched.readFrame()
        XCTAssertNil(end)
    }

    func testMetadataSubscriptionReadsFrameHeaders() async throws {
        let broadcast = try BroadcastProducer()
        let track = try broadcast.publishTrack(name: "video", info: TrackInfo(maxAge: 0, timescale: 1_000_000))
        let meta = try await broadcast.consume().subscribeMeta(name: "video")
        XCTAssertEqual(meta.info().timescale, 1_000_000)

        let group = try track.appendGroup(start: micros(0))
        try group.writeFrame(Data("key".utf8), timestamp: micros(0))
        try group.writeFrame(Data(), timestamp: micros(1))
        try group.finish()
        try track.finish()

        var starts: [UInt64] = []
        var headers: [FrameHeader] = []
        for try await group in meta {
            starts.append(group.start)
            for try await header in group {
                headers.append(header)
            }
        }
        XCTAssertEqual(starts, [0])
        XCTAssertEqual(headers.map(\.size), [3, 0])
        XCTAssertEqual(headers.map(\.timestamp), [micros(0), micros(1)])
    }

    func testJsonSnapshotRoundTrip() async throws {
        struct Status: Codable, Equatable {
            let state: String
            let viewers: Int
        }

        let broadcast = try BroadcastProducer()
        let producer = try Json.SnapshotProducer<Status>(
            broadcast: broadcast,
            track: try broadcast.publishTrack(
                name: "status", info: TrackInfo(maxAge: 0, timescale: 1_000_000, compression: Compression.deflate())))
        let consumer = try Json.SnapshotConsumer<Status>(
            broadcast: try broadcast.consume(), name: "status")

        for viewers in 1...4 {
            try producer.update(Status(state: "live", viewers: viewers), timestamp: micros(UInt64(viewers) * 1000))
        }

        // `next` yields every state in order; `latest` skips the buffered third.
        let first = try await consumer.next()
        XCTAssertEqual(first, Json.Timed(value: Status(state: "live", viewers: 1), timestamp: micros(1000)))
        let second = try await consumer.next()
        XCTAssertEqual(second, Json.Timed(value: Status(state: "live", viewers: 2), timestamp: micros(2000)))
        let latest = try await consumer.latest()
        XCTAssertEqual(latest, Json.Timed(value: Status(state: "live", viewers: 4), timestamp: micros(4000)))

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
            broadcast: broadcast,
            track: try broadcast.publishTrack(name: "events", info: TrackInfo(maxAge: 0, timescale: 1_000_000)))
        let consumer = try Json.StreamConsumer<Event>(
            track: try await broadcast.consume().subscribeTrack(name: "events"))

        for n in 0..<3 {
            try producer.append(Event(n: n), timestamp: micros(UInt64(n) * 1000))
            let record = try await consumer.next()
            XCTAssertEqual(record, Json.Timed(value: Event(n: n), timestamp: micros(UInt64(n) * 1000)))
        }

        consumer.cancel()
        try producer.finish()
        try broadcast.close()
    }

    func testJsonWindowRoundTrip() async throws {
        struct Event: Codable, Equatable {
            let n: Int
        }

        let broadcast = try BroadcastProducer()
        let producer = try Json.WindowProducer<Event>(
            track: try broadcast.publishTrack(
                name: "log", info: TrackInfo(maxAge: 0, timescale: 1_000_000, compression: Compression.deflate()))
        )
        let consumer = try Json.WindowConsumer<Event>(
            track: try await broadcast.consume().subscribeTrack(name: "log"))

        for n in 0..<3 {
            try producer.push(Event(n: n), timestamp: micros(UInt64(n) * 1000))
        }
        try producer.pop(2, timestamp: micros(3000))

        for n in 0..<3 {
            let event = try await consumer.next()
            XCTAssertEqual(
                event,
                Json.Timed(value: .push(index: UInt64(n), value: Event(n: n)), timestamp: micros(UInt64(n) * 1000)))
        }
        let pop = try await consumer.next()
        XCTAssertEqual(pop, Json.Timed(value: .pop(0..<2), timestamp: micros(3000)))

        // The cut's new group restates record 2, which this reader already has.
        try producer.cut()
        try producer.push(Event(n: 3), timestamp: micros(4000))
        let pushed = try await consumer.next()
        XCTAssertEqual(pushed, Json.Timed(value: .push(index: 3, value: Event(n: 3)), timestamp: micros(4000)))

        try producer.finish()
        let end = try await consumer.next()
        XCTAssertNil(end)
        try broadcast.close()
    }

    func testJsonProducersReportDemand() async throws {
        let broadcast = try BroadcastProducer()
        let timed = TrackInfo(maxAge: 0, timescale: 1_000_000)
        let snapshot = try Json.SnapshotProducer<[String: Int]>(
            broadcast: broadcast, track: try broadcast.publishTrack(name: "status", info: timed))
        let stream = try Json.StreamProducer<[String: Int]>(
            broadcast: broadcast, track: try broadcast.publishTrack(name: "events", info: timed))
        let snapshotDemand = try snapshot.demand()
        let streamDemand = try stream.demand()
        XCTAssertEqual(snapshotDemand.name, "status")
        XCTAssertFalse(snapshotDemand.wanted)
        let consumer = try broadcast.consume()

        let snapshotConsumer = try Json.SnapshotConsumer<[String: Int]>(
            broadcast: consumer, name: "status")
        let streamConsumer = try Json.StreamConsumer<[String: Int]>(
            track: try await consumer.subscribeTrack(name: "events"))
        while try await !snapshotDemand.wantedChanged() {}
        while try await !streamDemand.wantedChanged() {}
        XCTAssertTrue(snapshotDemand.wanted)

        snapshotConsumer.cancel()
        streamConsumer.cancel()
        while try await snapshotDemand.wantedChanged() {}
        while try await streamDemand.wantedChanged() {}

        try broadcast.close()
    }

    /// Released once unwanted, a track ends; a reader that joined since is served by the next
    /// request for it.
    func testReleaseEndsAnOnDemandTrack() async throws {
        let broadcast = try BroadcastProducer()
        let requests = try broadcast.requests()

        var idle: TrackProducer? = try broadcast.publishTrack(name: "idle")
        let idleDemand = try idle!.demand()
        while try await idleDemand.wantedChanged() {}
        idle = nil
        do {
            _ = try await idleDemand.wantedChanged()
            XCTFail("the watcher outlived its released track")
        } catch MoqError.Closed {}

        var track: TrackProducer? = try broadcast.publishTrack(name: "status")
        let demand = try track!.demand()
        while try await demand.wantedChanged() {}
        let late = try await broadcast.consume().subscribeTrack(name: "status")
        track = nil

        let request = try await requests.requestedTrack()
        XCTAssertEqual(try request.name, "status")
        let next = try request.accept()
        try next.writeFrame(Data("served again".utf8), timestamp: .unscaled(0))
        let frame = try await late.readFrame()
        XCTAssertEqual(frame?.payload, Data("served again".utf8))
        late.cancel()
        try broadcast.close()
    }

    /// A native encoder serves a track asked for again after its producer was released while
    /// read, published with the same settings.
    func testNativeEncodersServeARenewal() async throws {
        let broadcast = try BroadcastProducer()
        let requests = try broadcast.requests()
        let catalog = try Media.CatalogProducer(broadcast: broadcast)

        let mic = AudioEncoderInput(format: .f32, sampleRate: 48_000, channels: 1)
        let opus = AudioEncoderOutput(codec: AudioCodec.opus())
        var audio: Audio.Producer? = try Audio.Producer(
            catalog: catalog, input: mic, output: opus, track: "mic")
        var late = try await broadcast.consume().subscribeTrack(name: "mic")
        audio = nil
        var request = try await requests.requestedTrack()
        XCTAssertEqual(try request.name, "mic")
        audio = try Audio.Producer.requested(
            catalog: catalog, request: request, input: mic, output: opus)
        for index in 0..<3 {
            try audio!.write(
                AudioFrame(timestamp: micros(UInt64(index) * 20_000), data: Data(count: 960 * 4)))
        }
        var frame = try await late.readFrame()
        XCTAssertNotNil(frame)
        late.cancel()

        let camera = VideoEncoderInput(
            format: .rgba(), width: 320, height: 240,
            framerate: Video.FrameRate(numerator: 30, denominator: 1))
        let h264 = VideoEncoderOutput(codec: VideoCodec.h264(), kind: .software)
        var video: Video.Producer? = try Video.Producer(
            catalog: catalog, input: camera, output: h264, track: "camera")
        late = try await broadcast.consume().subscribeTrack(name: "camera")
        video = nil
        request = try await requests.requestedTrack()
        XCTAssertEqual(try request.name, "camera")
        video = try Video.Producer.requested(
            catalog: catalog, request: request, input: camera, output: h264)
        try video!.write(
            VideoFrame(timestamp: micros(0), data: Data(repeating: 0x80, count: 320 * 240 * 4)))
        try video!.finish()
        frame = try await late.readFrame()
        XCTAssertNotNil(frame)
        late.cancel()
        try broadcast.close()
    }

    /// A released typed producer hands a reader that joined since to the next request, like a
    /// raw one.
    func testJsonReleaseServesALateReader() async throws {
        let broadcast = try BroadcastProducer()
        let requests = try broadcast.requests()
        let info = TrackInfo(maxAge: nil, timescale: 1_000_000)
        var producer: Json.StreamProducer<String>? = try Json.StreamProducer<String>(
            broadcast: broadcast, track: try broadcast.publishTrack(name: "events", info: info))
        let demand = try producer!.demand()
        while try await demand.wantedChanged() {}

        let late = try Json.StreamConsumer<String>(
            track: try await broadcast.consume().subscribeTrack(name: "events"))
        producer = nil

        let request = try await requests.requestedTrack()
        XCTAssertEqual(try request.name, "events")
        let next = try Json.StreamProducer<String>(broadcast: broadcast, track: try request.accept(info: info))
        try next.append("served again", timestamp: micros(0))
        let record = try await late.next()
        XCTAssertEqual(record, Json.Timed(value: "served again", timestamp: micros(0)))
        late.cancel()
        try broadcast.close()
    }

    func testScaleLessTrackCarriesGroupKey() async throws {
        let broadcast = try BroadcastProducer()
        let track = try broadcast.publishTrack(name: "untimed")
        let consumer = try track.consume()
        // A scale-less track has no time, so a frame names its group's key instead.
        XCTAssertThrowsError(try track.writeFrame(Data("timed".utf8), timestamp: micros(0)))
        try track.writeFrame(Data("payload".utf8), timestamp: .unscaled(0))
        let received = try await consumer.readFrame()
        let frame = try XCTUnwrap(received)
        XCTAssertEqual(frame.payload, Data("payload".utf8))
        XCTAssertEqual(frame.timestamp, .unscaled(0))
        XCTAssertNil(frame.timestamp.scale)
        XCTAssertNil(consumer.info().timescale)
        // A group's frames carry that group's key, and only it.
        let group = try track.createGroup(start: 1)
        XCTAssertThrowsError(try group.writeFrame(Data("other".utf8), timestamp: .unscaled(2)))
        try group.writeFrame(Data("grouped".utf8), timestamp: .unscaled(group.start))
        try group.finish()
        let grouped = try await consumer.readFrame()
        XCTAssertEqual(grouped?.timestamp, .unscaled(1))
        try track.finish()
        try broadcast.close()
    }

    func testTrackInfoMaxAge() throws {
        let broadcast = try BroadcastProducer()
        let omitted = try broadcast.publishTrack(name: "omitted").consume().info()
        XCTAssertEqual(omitted.maxAge, 0)
        let info = TrackInfo(maxAge: nil)
        let unlimited = try broadcast.publishTrack(name: "unlimited", info: info).consume().info()
        XCTAssertNil(unlimited.maxAge)
        try broadcast.close()
    }

    /// A refresh reads back as published, and a bare one asks for nothing.
    func testTrackInfoRefresh() throws {
        let broadcast = try BroadcastProducer()
        let info = TrackInfo(maxAge: 0, refresh: TrackRefresh(ratio: 2, interval: 0.5))
        let got = try broadcast.publishTrack(name: "refresh", info: info).consume().info().refresh
        XCTAssertEqual(got.ratio, 2)
        XCTAssertEqual(got.interval, 0.5)
        let plain = try broadcast.publishTrack(name: "plain").consume().info().refresh
        XCTAssertNil(plain.ratio)
        XCTAssertNil(plain.interval)
        try broadcast.close()
    }

    /// The first subscriber of a track with no group asks for one.
    func testDemandRefreshWaitsForAGroupRequest() async throws {
        let broadcast = try BroadcastProducer()
        let track = try broadcast.publishTrack(
            name: "status", info: TrackInfo(maxAge: 0, refresh: TrackRefresh(ratio: 0)))
        let demand = try track.demand()
        let consumer = try track.consume()
        try await demand.refresh()
        _ = consumer
        try track.finish()
        try broadcast.close()
    }

    func testRawTrackTimestamps() async throws {
        let broadcast = try BroadcastProducer()
        let track = try broadcast.publishTrack(name: "events", info: TrackInfo(maxAge: 0, timescale: 1_000_000))
        let consumer = try track.consume()

        let payload = Data("ready".utf8)
        try track.writeFrame(payload, timestamp: micros(12_345))

        let frame = try await consumer.readFrame()
        XCTAssertEqual(frame?.payload, payload)
        XCTAssertEqual(frame?.timestamp, micros(12_345))

        let group = try track.appendGroup(start: micros(23_456))
        let groupConsumer = try group.consume()
        let groupPayload = Data("group".utf8)
        try group.writeFrame(groupPayload, timestamp: micros(23_456))
        try group.finish()

        let groupFrame = try await groupConsumer.readFrame()
        XCTAssertEqual(groupFrame?.payload, groupPayload)
        XCTAssertEqual(groupFrame?.timestamp, micros(23_456))

        try track.finish()
        try broadcast.close()
    }

    func testTimestampsAreExact() async throws {
        let broadcast = try BroadcastProducer()
        let track = try broadcast.publishTrack(name: "video", info: TrackInfo(maxAge: 0, timescale: 90_000))
        let consumer = try track.consume()

        // A present zero stays present, and one 90 kHz tick reads back as itself rather than
        // the 11 µs it floors to.
        for value: UInt64 in [0, 1] {
            let timestamp = Timestamp(value: value, scale: 90_000)
            try track.writeFrame(Data("frame".utf8), timestamp: timestamp)
            let frame = try await consumer.readFrame()
            XCTAssertEqual(frame?.timestamp, timestamp)
        }

        for invalid in [
            Timestamp(value: 0, scale: 0),
            Timestamp(value: 0, scale: 1 << 62),
            Timestamp(value: 1 << 61, scale: 90_000),
        ] {
            assertTimeOverflow(try track.writeFrame(Data("frame".utf8), timestamp: invalid))
        }

        try track.finish()
        try broadcast.close()
    }

    @available(macOS 13, iOS 16, *)
    func testTimestampDurations() throws {
        // Exact at the finest power-of-ten scale whose value fits.
        let atto: UInt64 = 1_000_000_000_000_000_000
        XCTAssertEqual(try Timestamp(.zero), Timestamp(value: 0, scale: atto))
        XCTAssertEqual(try Timestamp(.milliseconds(20)), Timestamp(value: atto / 50, scale: atto))
        XCTAssertEqual(try Timestamp(.seconds(3_600)), Timestamp(value: 3_600 * (atto / 10_000), scale: atto / 10_000))
        XCTAssertEqual(try Timestamp(.seconds((1 << 61) - 1)), Timestamp(value: (1 << 61) - 1, scale: 1))
        assertTimeOverflow(try Timestamp(.seconds(1 << 61)))
        assertTimeOverflow(try Timestamp(.seconds(-1)))
        // Three seconds of attoseconds passes 2^61, and the attosecond doesn't survive a coarser scale.
        assertTimeOverflow(try Timestamp(.seconds(3) + Duration(secondsComponent: 0, attosecondsComponent: 1)))

        // Back down to a whole attosecond: a 90 kHz tick is 11_111_111_111_111.1 as, 2/3 s ends in
        // ...6.6 as, and half an attosecond is 0.
        let tick = Duration(secondsComponent: 0, attosecondsComponent: 11_111_111_111_111)
        XCTAssertEqual(try Timestamp(value: 1, scale: 90_000).duration, tick)
        let twoThirds = Duration(secondsComponent: 0, attosecondsComponent: 666_666_666_666_666_666)
        XCTAssertEqual(try Timestamp(value: 2, scale: 3).duration, twoThirds)
        XCTAssertEqual(try Timestamp(value: 1, scale: 2 * atto).duration, .zero)
        XCTAssertEqual(try micros(1_500_000).duration, .milliseconds(1_500))
        XCTAssertEqual(try Timestamp(.seconds(3_600)).duration, .seconds(3_600))

        // A record outside what a write accepts is refused rather than converted.
        for invalid in [
            Timestamp(value: 0, scale: 0),
            Timestamp(value: 0, scale: 1 << 62),
            Timestamp(value: 1 << 61, scale: 90_000),
            // A scale-less timestamp has no time to convert.
            .unscaled(5),
        ] {
            assertTimeOverflow(try invalid.duration)
        }
    }

    func testReadFrameSkipsEmptyThenPopulatedGroups() async throws {
        let broadcast = try BroadcastProducer()
        let track = try broadcast.publishTrack(name: "status", info: TrackInfo(maxAge: 0, timescale: 1_000_000))
        let consumer = try track.consume()

        try track.appendGroup(start: micros(0)).finish()
        try track.appendGroup(start: micros(1_000)).finish()
        try track.writeFrame(Data("populated".utf8), timestamp: micros(2_000))

        let frame = try await consumer.readFrame()
        XCTAssertEqual(frame?.payload, Data("populated".utf8))
        XCTAssertEqual(frame?.timestamp, micros(2_000))

        try track.finish()
        try broadcast.close()
    }

    func testSparseGroupsAndKnownEnd() throws {
        let broadcast = try BroadcastProducer()
        let track = try broadcast.publishTrack(name: "sparse")
        let group = try track.createGroup(start: 2)
        XCTAssertEqual(group.start, 2)
        try group.finish()

        try track.finish(at: 5)
        try track.createGroup(start: 4).finish()
        XCTAssertThrowsError(try track.createGroup(start: 5))
        try track.finish()
        try broadcast.close()
    }

    /// Opus' 2.5 ms frame reaches the encoder, and a duration outside the Opus set
    /// is refused rather than silently rounded.
    func testEncodeAudioFrameDurations() throws {
        let broadcast = try BroadcastProducer()
        let input = AudioEncoderInput(format: .f32, sampleRate: 48_000, channels: 1)

        let catalog = try Media.CatalogProducer(broadcast: broadcast)
        let fine = try Audio.Producer(
            catalog: catalog,
            input: input,
            output: AudioEncoderOutput(codec: AudioCodec.opus(), frameDuration: 0.0025),
            track: "fine"
        )
        // 2.5 ms of silence at 48 kHz mono f32: exactly one encoded frame.
        try fine.write(AudioFrame(timestamp: micros(0), data: Data(count: 120 * 4)))
        try fine.finish()

        XCTAssertThrowsError(
            try Audio.Producer(
                catalog: catalog,
                input: input,
                output: AudioEncoderOutput(codec: AudioCodec.opus(), frameDuration: 0.002),
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
        let route = try origin.route(path: "audio-delay", hidden: true)
        defer { route.cancel() }
        let broadcast = route.broadcast
        let audio = try Audio.Producer(
            catalog: Media.CatalogProducer(broadcast: broadcast),
            input: AudioEncoderInput(format: .f32, sampleRate: 48_000, channels: 1),
            output: AudioEncoderOutput(codec: AudioCodec.opus(), ),
            track: "mic"
        )
        try route.announce()

        let consumer = try await origin.consume().request(path: "audio-delay")
        let catalogs = try await Media.CatalogConsumer.subscribe(broadcast: consumer)
        // XCTUnwrap takes an autoclosure, which can't hold an await.
        let nextCatalog = try await catalogs.next()
        let catalog = try XCTUnwrap(nextCatalog)
        let rendition = try XCTUnwrap(catalog.audio["mic"])

        // No max delay: a zero one skips a group once its successor is a warmup old by the
        // native wall clock, so a read that starts late under load never returns.
        let fixed = try await Audio.Consumer.subscribe(
            broadcast: consumer, id: "mic", rendition: rendition,
            output: AudioDecoderOutput(format: .f32, delay: Delay(min: 0.08, max: nil), estimate: false)
        )
        let estimated = try await Audio.Consumer.subscribe(
            broadcast: consumer, id: "mic", rendition: rendition,
            output: AudioDecoderOutput(format: .f32, delay: Delay(min: 0, max: nil))
        )
        XCTAssertEqual(fixed.delay, 0.08, accuracy: 1e-9)
        let initial = estimated.delay

        // Two 20 ms Opus frames of silence at 48 kHz mono.
        for i in 0..<2 {
            try audio.write(AudioFrame(timestamp: micros(UInt64(i) * 20_000), data: Data(count: 960 * 4)))
        }
        let fixedFrame = try await fixed.next()
        XCTAssertNotNil(fixedFrame)
        let estimatedFrame = try await estimated.next()
        XCTAssertNotNil(estimatedFrame)

        XCTAssertEqual(fixed.delay, 0.08, accuracy: 1e-9)
        XCTAssertNotEqual(estimated.delay, initial, "the estimate should move as frames are read")

        fixed.cancel()
        estimated.cancel()
        try audio.finish()
        try broadcast.close()
    }

    /// A decoded frame owns its picture: it converts to either CPU layout on
    /// demand and stays readable after its consumer is cancelled.
    func testDecodeVideoFrame() async throws {
        let origin = OriginProducer()
        let route = try origin.route(path: "video-decode-frame", hidden: true)
        defer { route.cancel() }
        let broadcast = route.broadcast
        let video = try Video.Producer(
            catalog: Media.CatalogProducer(broadcast: broadcast),
            input: VideoEncoderInput(
                format: .rgba(), width: 320, height: 240,
                framerate: Video.FrameRate(numerator: 30000, denominator: 1001)),
            output: VideoEncoderOutput(codec: VideoCodec.h264(), kind: .software),
            track: "camera"
        )
        try route.announce()

        // Seed the track so a subscriber joining below lands on encoded media.
        let rgba = Data(repeating: 0x80, count: 320 * 240 * 4)
        try video.cut()
        for i in 0..<10 {
            try video.write(VideoFrame(timestamp: micros(UInt64(i) * 33_333), data: rgba))
        }

        let consumer = try await origin.consume().request(path: "video-decode-frame")
        let catalogs = try await Media.CatalogConsumer.subscribe(broadcast: consumer)
        // XCTUnwrap takes an autoclosure, which can't hold an await.
        let nextCatalog = try await catalogs.next()
        let catalog = try XCTUnwrap(nextCatalog)
        let rendition = try XCTUnwrap(catalog.video["camera"])

        let decoder = try await Video.Consumer.subscribe(broadcast: consumer, id: "camera", rendition: rendition)

        // Keep the encoder fed so the decoder sees frames after it joined.
        for i in 10..<40 {
            try video.write(VideoFrame(timestamp: micros(UInt64(i) * 33_333), data: rgba))
        }

        let next = try await decoder.next()
        guard case .frame(let frame) = try XCTUnwrap(next) else {
            return XCTFail("expected a decoded frame")
        }
        decoder.cancel()
        XCTAssertNil(frame.surface())

        let planar = try frame.pixels(format: .i420())
        XCTAssertEqual(planar.count, Int(frame.width()) * Int(frame.height()) * 3 / 2)

        let packed = try frame.pixels(format: .rgba())
        XCTAssertEqual(packed.count, Int(frame.width()) * Int(frame.height()) * 4)
        XCTAssertTrue(stride(from: 3, to: packed.count, by: 4).allSatisfy { packed[$0] == 0xFF })

        try video.finish()
        try broadcast.close()
    }

    func testEncodeAudioWithOpusObject() throws {
        // The config retains the codec, so releasing either first must still encode.
        let input = AudioEncoderInput(format: .f32, sampleRate: 48_000, channels: 1)
        let silence = AudioFrame(timestamp: micros(0), data: Data(count: 960 * 4))

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

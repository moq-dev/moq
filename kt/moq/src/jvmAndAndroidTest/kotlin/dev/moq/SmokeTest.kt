package dev.moq

import dev.moq.media.*
import dev.moq.audio.Consumer as AudioConsumer
import dev.moq.audio.ReadFrame as AudioReadFrame
import dev.moq.audio.Producer as AudioProducer
import dev.moq.audio.ProducerOptions as AudioProducerOptions
import dev.moq.video.Consumer as VideoConsumer
import dev.moq.video.ReadFrame as VideoReadFrame
import dev.moq.video.Producer as VideoProducer
import dev.moq.video.ProducerOptions as VideoProducerOptions
import dev.moq.video.FrameRate as VideoFrameRate

import kotlinx.coroutines.async
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.toList
import kotlinx.coroutines.test.runTest
import dev.moq.json.SnapshotConfig
import dev.moq.json.SnapshotConsumer
import dev.moq.json.SnapshotProducer
import dev.moq.json.Timed
import dev.moq.json.WindowConfig
import dev.moq.json.WindowConsumer
import dev.moq.json.WindowEventPop
import dev.moq.json.WindowEventPush
import dev.moq.json.WindowProducer
import dev.moq.json.decode
import dev.moq.json.events
import dev.moq.json.push
import dev.moq.json.timestamp
import dev.moq.json.update
import dev.moq.json.valuesAs
import kotlinx.serialization.Serializable
import uniffi.moq.MoqException
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlin.time.Duration.Companion.milliseconds

@Serializable
private data class Status(val state: String)

/** The next announce event. */
private suspend fun AnnounceConsumer.nextRoute(): AnnounceEvent =
    checkNotNull(next()) { "announce stream ended" }

private fun opusHead(): ByteArray =
    "OpusHead".encodeToByteArray() + byteArrayOf(
        1,
        2,
        0,
        0,
        0x80.toByte(),
        0xbb.toByte(),
        0,
        0,
        0,
        0,
        0,
    )

class SmokeTest {
    @Test
    fun `destroyed object arguments are refused before native calls`() {
        val broadcast = BroadcastProducer()
        broadcast.destroy()

        val error = assertFailsWith<IllegalStateException> { CatalogProducer(broadcast) }
        assertTrue(error.message.orEmpty().contains("already been destroyed"))
    }

    @Test
    fun `container timebase belongs to one broadcast`() {
        BroadcastProducer().use { owner ->
            BroadcastProducer().use { other ->
                Timebase(owner).use { timebase ->
                    assertFailsWith<MoqException.Codec> {
                        ContainerProducer(other, ContainerInit(ContainerFormat.FMP4, byteArrayOf(), timebase))
                    }
                    owner.end()
                    assertFailsWith<MoqException.Closed> { Timebase(owner) }
                }
            }
        }
    }

    @Test
    fun `diagnostic errors expose their named reason`() {
        val error = MoqException.Transport(reason = "reset")
        assertEquals("reset", error.reason)
    }

    @Test
    fun `stream abort preserves protocol details`() = runTest {
        BroadcastProducer().use { broadcast ->
            val track = broadcast.publishTrack("errors", null)
            val producer = track.appendGroup()
            val group = broadcast.consume().fetchGroup("errors", 0uL, FetchGroupOptions())
            producer.abort(404u)
            val error = assertFailsWith<MoqException.Protocol> { group.readFrame() }
            assertEquals(ErrorScope.STREAM, error.details.scope)
            assertEquals(468u, error.details.code)
            assertEquals(ProtocolKind.APP, error.details.kind)
        }
    }

    /**
     * Exercises the [Moq.connect] facade end to end without a network: a bogus
     * dial failure reaches the returned session through [Session.closed]. Also proves
     * the native lib loads through the transitive `moq-ffi` dependency.
     * One-shot mode, since the default reconnect would retry the dial with
     * backoff instead of failing.
     */
    @Test
    fun `connect fails fast and surfaces a MoqException`() = runTest {
        val ex = assertFailsWith<MoqException> {
            Moq.connect("https://localhost:0/test", ClientConfig(tls = ClientTls(insecure = true), once = true)).use { moq ->
                moq.session.closed()
            }
        }
        assertTrue(
            ex.isShutdown || ex is MoqException.Connect || ex is MoqException.Url,
            "expected shutdown/connect/url error, got: $ex",
        )
    }

    /**
     * The WebSocket fallback knobs reach the native client: a QUIC-only dial with
     * no head start still fails fast, and an invalid value is refused up front
     * as a config error rather than at the dial.
     */
    @Test
    fun `connect accepts the websocket fallback knobs`() = runTest {
        assertFailsWith<MoqException> {
            Moq.connect(
                "https://localhost:0/test",
                ClientConfig(
                    tls = ClientTls(insecure = true),
                    once = true,
                    websocket = WebSocketConfig(enabled = false, delay = java.time.Duration.ZERO),
                ),
            ).use { moq -> moq.session.closed() }
        }
        assertFailsWith<MoqException.Config> {
            Moq.connect("https://localhost:0/test", ClientConfig(versions = listOf("moq-lite-99")))
        }
    }

    /** A settings UI reads its defaults and menus from the library rather than repeating them. */
    @Test
    fun `client defaults and versions come from the library`() {
        val defaults = uniffi.moq.moqClientDefaults()
        assertNotNull(defaults.quic.maxStreams)
        assertNotNull(defaults.quic.idleTimeout)
        assertNotNull(defaults.timeout)
        assertNull(defaults.quic.congestionControl)
        assertTrue(versions().isNotEmpty())
        val congestion: CongestionControl = CongestionControl.DELAY
        assertEquals(congestion, QuicConfig(congestionControl = congestion).congestionControl)
        // Dialing with the reported defaults is the same as dialing with none.
        Client(defaults).close()
    }

    /**
     * The `dev.moq` typealiases resolve to the FFI objects, and the wrapper
     * extensions apply to them. Constructing through an alias is enough to
     * confirm both at compile time + lib load at runtime.
     */
    @Test
    fun `origin alias constructs and consumes`() = runTest {
        OriginProducer(OriginConfig()).use { origin ->
            origin.consume().use { /* lifecycle smoke */ }
            origin.requests("").use { /* dynamic origin smoke */ }
        }
    }

    @Test
    fun `all public ffi records and handles have aliases`() {
        val hint: VideoHint = VideoHint(
            coded = Dimensions(1920u, 1080u),
            bitrate = 4_000_000uL,
            framerate = 60.0,
            optimizeForLatency = true,
        )
        val snapshot: SnapshotConfig = SnapshotConfig(deltas = true)
        val properties: VideoProperties = VideoProperties(rotation = 315.0)
        val backoff: Backoff = Backoff(
            initial = java.time.Duration.ofMillis(500),
            multiplier = 2u,
            max = java.time.Duration.ofSeconds(10),
            timeout = java.time.Duration.ZERO,
        )
        val client: ClientConfig = ClientConfig(
            tls = ClientTls(insecure = true),
            quic = QuicConfig(maxStreams = 4096uL),
            websocket = WebSocketConfig(enabled = false),
            backoff = backoff,
        )
        val server: ServerConfig = ServerConfig(tls = ServerTls(generate = listOf("localhost")))
        assertEquals(4096uL, client.quic.maxStreams)
        assertEquals(listOf("localhost"), server.tls.generate)
        val status: ConnectionPhase = ConnectionPhase.CONNECTED
        assertEquals(4_000_000uL, hint.bitrate)
        assertEquals(true, snapshot.deltas)
        assertNull(properties.display)
        assertNull(properties.flip)
        assertEquals(java.time.Duration.ofMillis(500), backoff.initial)
        assertEquals(ConnectionPhase.CONNECTED, status)
    }

    @Test
    fun `closing a broadcast ends it, and closing again is a no-op`() = runTest {
        val broadcast = BroadcastProducer()
        val consumer = broadcast.consume()
        // AutoCloseable.close() releases the last producer handle, which ends the broadcast.
        broadcast.close()
        broadcast.close()
        assertFailsWith<MoqException> { consumer.subscribeTrack("events", null) }
    }

    @Test
    fun `ending a broadcast ends it while a dynamic handle remains`() = runTest {
        BroadcastProducer().use { broadcast ->
            broadcast.requests().use {
                val consumer = broadcast.consume()
                // Releasing the producer alone would leave the dynamic handle holding it open.
                broadcast.end()
                broadcast.end()
                assertFailsWith<MoqException> { consumer.subscribeTrack("events", null) }
                assertFailsWith<MoqException> { broadcast.publishTrack("events", null) }
            }
        }
    }

    @Test
    fun `a finished catalog leaves the broadcast up until it ends`() {
        BroadcastProducer().use { broadcast ->
            val catalog = CatalogProducer(broadcast)
            broadcast.publishTrack("status", null).finish()
            catalog.finish()
            // Finishing the content leaves the broadcast up until it ends.
            broadcast.consume().close()
            broadcast.end()
            assertFailsWith<MoqException> { catalog.finish() }
        }
    }

    @Test
    fun `broadcast updates shared video properties`() {
        BroadcastProducer().use { broadcast ->
            CatalogProducer(broadcast).setVideoProperties(VideoProperties(rotation = 315.0))
        }
    }

    @Test
    fun `broadcast consumer fetches cached group`() = runTest {
        BroadcastProducer().use { broadcast ->
            val track = broadcast.publishTrack("events", TrackInfo(timescale = 1_000_000uL))
            val group = track.appendGroup()
            group.writeFrame(Frame(payload = "cached".encodeToByteArray(), timestampUs = 0uL))
            group.finish()

            val fetched = broadcast.consume().fetchGroup(
                "events",
                0uL,
                FetchGroupOptions(priority = 3),
            )
            assertEquals(0uL, fetched.sequence())
            assertEquals("cached", fetched.readFrame()?.payload?.decodeToString())
            assertNull(fetched.readFrame())
        }
    }

    @Test
    fun `a default subscription skips superseded groups at once`() {
        assertEquals(java.time.Duration.ZERO, Subscription.default().maxDelay)
        assertNull(Subscription(maxDelay = null).maxDelay)
    }

    @Test
    fun `an ascending subscription reads a cached backlog oldest first`() = runTest {
        BroadcastProducer().use { broadcast ->
            val track = broadcast.publishTrack("backlog", TrackInfo(timescale = 1_000_000uL))
            for (ms in 0uL until 3uL) {
                val group = track.appendGroup()
                group.writeFrame(Frame(payload = "x".encodeToByteArray(), timestampUs = ms * 1_000uL))
                group.finish()
            }
            val consumer = track.consume(
                Subscription(
                    maxDelay = java.time.Duration.ofSeconds(10),
                    ranges = listOf(Range(start = AnchorRelative(groups = 2uL))),
                    order = Order.ASCENDING,
                ),
            )
            val sequences = List(3) { checkNotNull(consumer.recvGroup()).sequence() }
            assertEquals(listOf(0uL, 1uL, 2uL), sequences)
        }
    }

    @Test
    fun `readFrame skips empty then populated groups`() = runTest {
        BroadcastProducer().use { broadcast ->
            val track = broadcast.publishTrack("status", TrackInfo(timescale = 1_000_000uL))
            val consumer = track.consume(null)
            track.appendGroup().finish()
            track.appendGroup().finish()
            track.writeFrame(Frame(payload = "populated".encodeToByteArray(), timestampUs = 2_000uL))
            val frame = consumer.readFrame()
            assertEquals("populated", frame?.payload?.decodeToString())
            assertEquals(2_000uL, frame?.timestampUs)
        }
    }

    @Test
    fun `raw writes without timestamps stay untimed`() = runTest {
        BroadcastProducer().use { broadcast ->
            val track = broadcast.publishTrack("untimed", null)
            val consumer = track.consume(null)
            track.writeFrame(Frame(payload = "payload".encodeToByteArray()))
            val frame = consumer.readFrame()
            assertEquals("payload", frame?.payload?.decodeToString())
            assertNull(frame?.timestampUs)
            assertNull(consumer.info().timescale)
            track.finish()
        }
    }

    @Test
    fun `catalog handle closes with its broadcast`() {
        val broadcast = BroadcastProducer()
        val catalog = CatalogProducer(broadcast)
        catalog.setSection("app", "{\"value\":42}")
        catalog.now()
        broadcast.close()
        assertFailsWith<MoqException.Closed> { catalog.removeSection("app") }
        assertFailsWith<MoqException.Closed> { catalog.now() }
        catalog.close()
    }

    /** A fetched media group streams its decoded frames and then completes. */
    @Test
    fun `media group helper streams fetched frames`() = runTest {
        BroadcastProducer().use { broadcast ->
            val media = dev.moq.media.TrackProducer.audio(broadcast, Named(null), AudioInit(format = AudioFormat.OPUS, data = opusHead()))
            val consumer = broadcast.consume()
            val (name, audio) = catalog(consumer).audio.entries.single()

            media.writeFrame(Frame(payload = "opus frame".encodeToByteArray(), timestampUs = 5_000_000uL))

            // Fetch while the track is still published: finishing the media producer
            // unpublishes it, and the fetch would then miss with NotFound.
            val fetched = dev.moq.media.GroupConsumer.fetch(consumer, dev.moq.media.GroupConfig(rendition = dev.moq.media.AudioRendition(name, audio), sequence = 0uL, options = FetchGroupOptions(priority = 3)))

            // Close the group so the fetched stream terminates instead of waiting for more.
            media.finish()

            fetched.use {
                assertEquals(0uL, it.sequence())
                val frames = it.frames().toList()
                assertEquals(1, frames.size)
                val frame = frames.single()
                assertEquals("opus frame", frame.payload.decodeToString())
                assertEquals(5_000_000uL, frame.timestampUs)
            }
        }
    }

    /** The typed JSON helpers round-trip a `@Serializable` value. */
    /**
     * Video has the same Flow audio does. Decoding real frames needs an encoder
     * backend, so this pins the extension's shape; the decode itself is covered
     * by the interop test (`just test interop`).
     */
    @Test
    fun `video consumer exposes a reads flow like audio does`() {
        val video: (dev.moq.video.Consumer) -> Flow<dev.moq.video.Read> = dev.moq.video.Consumer::reads
        val audio: (dev.moq.audio.Consumer) -> Flow<dev.moq.audio.Read> = dev.moq.audio.Consumer::reads

        assertNotNull(video)
        assertNotNull(audio)
    }

    /** Microsecond timestamps read back as a Duration. */
    @Test
    fun `microsecond timestamps read back as durations`() {
        assertEquals(20.milliseconds, Frame(payload = ByteArray(0), timestampUs = 20_000uL).timestamp)
        assertNull(AudioEncoderOutput(codec = AudioCodec.opus()).frameDuration)
    }

    @Test
    fun `typed json snapshot round-trips a serializable value`() = runTest {
        BroadcastProducer().use { broadcast ->
            val config = SnapshotConfig(deltas = true)
            val track = broadcast.publishTrack("status", TrackInfo(timescale = 1_000_000uL))
            val producer = SnapshotProducer(broadcast, track, config)
            val consumer = SnapshotConsumer(broadcast.consume().subscribeTrack("status", null))
            for (n in 1..4) producer.update(Status(state = "live$n"), timestampUs = n.toULong() * 1_000uL)

            // `next` yields every state in order; `latest` skips the buffered third.
            assertEquals(Timed(Status(state = "live1"), 1.milliseconds), consumer.next()?.decode<Status>())
            assertEquals(Timed(Status(state = "live2"), 2.milliseconds), consumer.next()?.decode<Status>())
            assertEquals(Timed(Status(state = "live4"), 4.milliseconds), consumer.latest()?.decode<Status>())
        }
    }

    @Test
    fun `json window yields pushes and pops in order`() = runTest {
        BroadcastProducer().use { broadcast ->
            val config = WindowConfig()
            val track = broadcast.publishTrack("log", TrackInfo(timescale = 1_000_000uL, compress = true))
            val producer = WindowProducer(track, config)
            val consumer = WindowConsumer(broadcast.consume().subscribeTrack("log", null))
            for (n in 1..3) producer.push(Status(state = "live$n"), timestampUs = n.toULong() * 1_000uL)
            producer.pop(2uL, 4_000uL)

            for (n in 0..2) {
                val timed = checkNotNull(consumer.next())
                val push = assertIs<WindowEventPush>(timed.event)
                assertEquals(n.toULong(), push.index)
                assertEquals(Status(state = "live${n + 1}"), push.decode<Status>())
                assertEquals((n + 1).milliseconds, timed.timestamp)
            }
            val pop = checkNotNull(consumer.next())
            assertEquals(WindowEventPop(0uL, 2uL), pop.event)
            assertEquals(4.milliseconds, pop.timestamp)

            // The cut's new group restates record 2, which this reader already has.
            producer.cut()
            producer.push(Status(state = "live4"), timestampUs = 5_000uL)
            producer.finish()
            val events = consumer.events().toList()
            assertEquals(1, events.size)
            val push = assertIs<WindowEventPush>(events[0].event)
            assertEquals(3uL, push.index)
            assertEquals(Status(state = "live4"), push.decode<Status>())
        }
    }

    @Test
    fun `json producer demand follows subscribers`() = runTest {
        BroadcastProducer().use { broadcast ->
            val config = SnapshotConfig(deltas = false)
            val producer = SnapshotProducer(broadcast, broadcast.publishTrack("status", null), config)
            val demand: TrackDemand = producer.demand()
            assertEquals("status", demand.name())
            assertEquals(null, demand.wanted())

            val consumer = SnapshotConsumer(broadcast.consume().subscribeTrack("status", null))
            while (demand.wantedChanged() == null) {}
            consumer.cancel()
            while (demand.wantedChanged() != null) {}
        }
    }

    @Test
    fun `cancelling a flow collection cancels its native handle`() = runTest {
        BroadcastProducer().use { broadcast ->
            val config = SnapshotConfig(deltas = false)
            val producer = SnapshotProducer(broadcast, broadcast.publishTrack("status", null), config)
            val demand: TrackDemand = producer.demand()
            producer.update(Status(state = "live"))

            val consumer = SnapshotConsumer(broadcast.consume().subscribeTrack("status", null))
            while (demand.wantedChanged() == null) {}
            // `first` cancels the collection, which must release the subscription without an explicit cancel.
            assertEquals(Status(state = "live"), consumer.valuesAs<Status>().first().value)
            while (demand.wantedChanged() != null) {}
        }
    }

    /**
     * A pre-encoded `String` must reach the wire untouched: the member overload
     * wins over the reified extension, which would otherwise double-encode it
     * into a JSON string literal.
     */
    @Test
    fun `raw json string passes through unencoded`() = runTest {
        BroadcastProducer().use { broadcast ->
            val config = SnapshotConfig(deltas = false)
            val producer = SnapshotProducer(broadcast, broadcast.publishTrack("status", null), config)
            producer.update("""{"state":"raw"}""")

            val consumer = SnapshotConsumer(broadcast.consume().subscribeTrack("status", null))
            assertEquals(Timed(Status(state = "raw"), null), consumer.valuesAs<Status>().first())
        }
    }

    @Test
    fun `server listens, publishes, and streams requests`() = runTest {
        Server.listen(ServerConfig(bind = "127.0.0.1:0", tls = ServerTls(generate = listOf("localhost")))).use { server ->
            assertTrue(server.localAddr.startsWith("127.0.0.1:"), "bound: ${server.localAddr}")

            val fingerprints = server.certFingerprints()
            assertEquals(1, fingerprints.size)
            assertEquals(64, fingerprints[0].length)

            server.createBroadcast("live").use { publication ->
                publication.announce(AnnounceRoute())
                publication.unannounce()
            }
        }
    }

    @Test
    fun `sessions advertise the wrapper ahead of moq-ffi`() = runTest {
        Server.listen(ServerConfig(bind = "127.0.0.1:0", versions = listOf("moq-lite-07-wip"), tls = ServerTls(generate = listOf("localhost")))).use { server ->
            Moq.connect("https://${server.localAddr}", ClientConfig(versions = listOf("moq-lite-07-wip"), tls = ClientTls(insecure = true))).use {
                server.requests().first().use { request ->
                    val advertised = request.peerImplementation()
                    assertTrue(advertised?.startsWith("moq-kt/") == true, "advertised: $advertised")
                }
            }
        }
    }

    @Test
    fun `closing a server releases its port`() = runTest {
        val first = Server.listen(ServerConfig(bind = "127.0.0.1:0", tls = ServerTls(generate = listOf("localhost"))))
        val addr = first.localAddr
        first.close()

        // No retry: close() released the listening socket before returning.
        Server.listen(ServerConfig(bind = addr, tls = ServerTls(generate = listOf("localhost")))).use { rebound ->
            assertEquals(addr, rebound.localAddr)
        }
    }

    @Test
    fun `a broadcast is reachable only while announced`() = runTest {
        OriginProducer(OriginConfig()).use { origin ->
            origin.createBroadcast("live").use { publication ->
                publication.broadcast().use { it.publishTrack("events", null) }
                val consumer = origin.consume()
                assertFailsWith<MoqException> { consumer.request("live") }

                publication.announce(AnnounceRoute())
                val announced = consumer.announced(AnnounceConfig())
                val first = assertIs<AnnounceEventStart>(announced.nextRoute())
                assertEquals("live", first.announce.prefix)

                publication.unannounce()
                val retracted = assertIs<AnnounceEventEnd>(announced.nextRoute())
                assertEquals("live", retracted.announce.prefix)
                assertFailsWith<MoqException> { consumer.request("live") }

                publication.announce(AnnounceRoute())
                assertIs<AnnounceEventStart>(announced.nextRoute())
                consumer.request("live")
            }
        }
    }

    @Test
    fun `closing a publication keeps its route until the broadcast ends`() = runTest {
        OriginProducer(OriginConfig()).use { origin ->
            val consumer = origin.consume()
            val announced = consumer.announced(AnnounceConfig())
            val publication = origin.createBroadcast("live")
            publication.broadcast().use { broadcast ->
                publication.use { it.announce(AnnounceRoute()) }
                assertIs<AnnounceEventStart>(announced.nextRoute())
                consumer.request("live")

                broadcast.end()
                assertIs<AnnounceEventEnd>(announced.nextRoute())
                assertFailsWith<MoqException> { consumer.request("live") }
            }
        }
    }

    @Test
    fun `a stale publication leaves its replacement alone`() = runTest {
        OriginProducer(OriginConfig()).use { origin ->
            origin.createBroadcast("live").use { stale ->
                stale.announce(AnnounceRoute())
                stale.broadcast().use { it.end() }

                origin.createBroadcast("live").use { replacement ->
                    replacement.announce(AnnounceRoute())
                    assertFailsWith<MoqException.Closed> { stale.announce(AnnounceRoute()) }
                    assertFailsWith<MoqException.Closed> { stale.unannounce() }
                    origin.consume().request("live")
                    replacement.broadcast().use { it.end() }
                }
            }
        }
    }

    @Test
    fun `a new epoch restarts announce consumers`() = runTest {
        OriginProducer(OriginConfig()).use { origin ->
            val announced = origin.consume().announced(AnnounceConfig())
            origin.createBroadcast("epoch").use { publication ->
                val first = mintEpoch()
                publication.announce(AnnounceRoute(epoch = first))
                val start = assertIs<AnnounceEventStart>(announced.nextRoute())
                assertEquals(first, start.announce.route.epoch)

                val second = mintEpoch()
                publication.announce(AnnounceRoute(epoch = second))
                val restart = assertIs<AnnounceEventRestart>(announced.nextRoute())
                assertEquals(second, restart.announce.route.epoch)

                assertFailsWith<MoqException> { publication.announce(AnnounceRoute(epoch = "not-an-epoch")) }
            }
        }
    }

    @Test
    fun `announced pattern reports captures`() = runTest {
        OriginProducer(OriginConfig()).use { origin ->
            val announced = origin.consume().announced(AnnounceConfig(prefix = "room", filter = "*/chat"))
            origin.createBroadcast("room/alice/chat").use { publication ->
                publication.announce(AnnounceRoute())
                val update = assertIs<AnnounceEventStart>(announced.nextRoute())
                assertEquals("room/alice/chat", update.announce.prefix)
                assertEquals(listOf("alice"), update.announce.captures)
            }
        }
    }

    @Test
    fun `dynamic serves a request under a prefix`() = runTest {
        OriginProducer(OriginConfig()).use { origin ->
            origin.requests("live").use { dynamic ->
                val pending = async {
                    origin.consume().request("live/cam")
                }
                val request = dynamic.requestedBroadcast()
                assertEquals("live/cam", request.path())
                request.prepare(mintEpoch()).use { prepared ->
                    prepared.broadcast().use { fill ->
                        prepared.publish().use { publication ->
                            pending.await()
                            // The publication fills the broadcast it prepared: ending one ends the other.
                            publication.broadcast().use { it.end() }
                            assertFailsWith<MoqException.Closed> { fill.publishTrack("late", null) }
                        }
                    }
                }
            }
        }
    }

    @Test
    fun `raw track supports sparse groups and a known end`() {
        BroadcastProducer().use { broadcast ->
            val track = broadcast.publishTrack("sparse", null)
            track.createGroup(2uL).finish()
            track.finishAt(5uL)
            track.createGroup(4uL).finish()
            assertFailsWith<MoqException> { track.createGroup(5uL) }
            track.finish()
        }
    }

    /**
     * Opus' 2.5 ms frame reaches the encoder, and a duration outside the Opus
     * set is refused rather than silently rounded.
     */
    @Test
    fun `encode audio honors the opus frame duration set`() {
        val input = AudioEncoderInput(format = AudioSampleFormat.F32, sampleRate = 48_000u, channels = 1u)

        BroadcastProducer().use { broadcast ->
            val catalog = CatalogProducer(broadcast)
            val fine = AudioProducer(
                catalog,
                AudioProducerOptions(
                    input = input,
                    output = AudioEncoderOutput(codec = AudioCodec.opus(),  frameDuration = java.time.Duration.ofNanos(2_500_000)),
                    track = "fine",
                ),
            )
            // 2.5 ms of silence at 48 kHz mono f32: exactly one encoded frame.
            fine.write(AudioFrame(timestampUs = 0uL, data = ByteArray(120 * 4)))
            fine.finish()

            assertFailsWith<MoqException.Audio> {
                AudioProducer(
                    catalog,
                    AudioProducerOptions(
                        input = input,
                        output = AudioEncoderOutput(codec = AudioCodec.opus(),  frameDuration = java.time.Duration.ofMillis(2)),
                        track = "coarse",
                    ),
                )
            }
        }
    }

    /**
     * Both sides read the grant their peer gave them, and a peer that verifies no tokens
     * of its own refuses another one as unsupported.
     */
    @Test
    fun `session auth reads the grant`() = runTest {
        val version = listOf("moq-lite-07-wip")
        Server.listen(
            ServerConfig(bind = "127.0.0.1:0", versions = version, tls = ServerTls(generate = listOf("localhost"))),
        ).use { server ->
            // Not requests().first(): ending that collection cancels the server.
            val accepted = async { server.server.accept()!!.accept() }
            val moq = Moq.connect(
                "https://${server.localAddr}",
                ClientConfig(bind = "127.0.0.1:0", versions = version, tls = ClientTls(insecure = true), once = true),
            )
            moq.use {
                val everything = Grant(publish = listOf("**"), subscribe = listOf("**"), expires = null)
                assertEquals(everything, moq.grants().updates().first { it != null })
                val session = accepted.await()
                assertEquals(everything, session.grants().updates().first { it != null })
                assertNull(everything.expires)
                assertNull(session.tokens(), "an accepted session presents no tokens")

                moq.tokens().set(listOf("extra"))
                assertFailsWith<MoqException.Unsupported> { moq.tokens().answered() }
                assertEquals(everything, moq.grants().grant())
                session.cancel(0u)
            }
        }
    }

    /** The returned Session owns its pending dial even after Client cancellation. */
    @Test
    fun `session owns and cancels its first dial`() = runTest {
        Server.listen(ServerConfig(bind = "127.0.0.1:0", tls = ServerTls(generate = listOf("localhost")))).use { server ->
            val client = Client(ClientConfig(bind = "127.0.0.1:0", tls = ClientTls(insecure = true), once = true))
            val session = client.connect("https://${server.localAddr}")
            client.cancel()
            assertEquals(0uL, session.state().connects)
            session.cancel(0u)
            session.closed()
            assertEquals(ConnectionPhase.CLOSED, session.state().phase)
        }
    }

    /** A fixed playout delay is reported back as is, and an estimated one moves as frames are read. */
    @Test
    fun `audio consumer reports its playout delay`() = runTest {
        OriginProducer(OriginConfig()).use { origin ->
            origin.createBroadcast("audio-delay").use { publication ->
                publication.broadcast().use { broadcast ->
                    val audio = AudioProducer(
                        CatalogProducer(broadcast),
                        AudioProducerOptions(
                            input = AudioEncoderInput(format = AudioSampleFormat.F32, sampleRate = 48_000u, channels = 1u),
                            output = AudioEncoderOutput(codec = AudioCodec.opus(), ),
                            track = "mic",
                        ),
                    )
                    publication.announce(AnnounceRoute())

                    val consumer = origin.consume().request("audio-delay")
                    val catalog = CatalogConsumer.subscribe(consumer).next()!!
                    val rendition = catalog.audio["mic"]!!

                    val fixedOutput = AudioDecoderOutput(format = AudioSampleFormat.F32, delay = java.time.Duration.ofMillis(80))
                    val fixed = AudioConsumer.subscribe(consumer, "mic", rendition, fixedOutput)
                    val estimated = AudioConsumer.subscribe(consumer, "mic", rendition, AudioDecoderOutput(format = AudioSampleFormat.F32))
                    assertEquals(java.time.Duration.ofMillis(80), fixed.delay())
                    val initial = estimated.delay()

                    // Two 20 ms Opus frames of silence at 48 kHz mono.
                    for (i in 0 until 2) {
                        audio.write(AudioFrame(timestampUs = i.toULong() * 20_000uL, data = ByteArray(960 * 4)))
                    }
                    assertIs<AudioReadFrame>(fixed.next())
                    assertIs<AudioReadFrame>(estimated.next())

                    assertEquals(java.time.Duration.ofMillis(80), fixed.delay())
                    assertTrue(estimated.delay() != initial, "the estimate should move as frames are read")

                    fixed.cancel()
                    estimated.cancel()
                    audio.finish()
                }
            }
        }
    }

    @Test
    fun `decoded video frame owns its picture`() = runTest {
        OriginProducer(OriginConfig()).use { origin ->
            origin.createBroadcast("video-decode-frame").use { publication ->
                publication.broadcast().use { broadcast ->
                    val video = VideoProducer(
                        CatalogProducer(broadcast),
                        VideoProducerOptions(
                            input = VideoEncoderInput(format = VideoPixelFormat.RGBA, width = 320u, height = 240u, framerate = VideoFrameRate(30000u, 1001u)),
                            output = VideoEncoderOutput(codec = VideoCodec.H264,  kind = softwareEncoder),
                            track = "camera",
                            bandwidth = null,
                        ),
                    )
                    publication.announce(AnnounceRoute())

                    // Seed the track so a subscriber joining below lands on encoded media.
                    val rgba = ByteArray(320 * 240 * 4) { 0x80.toByte() }
                    video.cut()
                    for (i in 0 until 10) {
                        video.write(VideoFrame(timestampUs = i.toULong() * 33_333uL, data = rgba))
                    }

                    val consumer = origin.consume().request("video-decode-frame")
                    val catalog = CatalogConsumer.subscribe(consumer).next()!!
                    val rendition = catalog.video["camera"]!!

                    val decoder = VideoConsumer.subscribe(consumer, "camera", rendition, VideoDecoderOutput())

                    // Keep the encoder fed so the decoder sees frames after it joined.
                    for (i in 10 until 40) {
                        video.write(VideoFrame(timestampUs = i.toULong() * 33_333uL, data = rgba))
                    }

                    assertIs<VideoReadFrame>(decoder.next()).frame.use { frame ->

                        // The frame outlives its consumer's cancellation.
                        decoder.cancel()

                        val planar = frame.pixels(VideoPixelFormat.I420)
                        assertEquals(frame.width().toInt() * frame.height().toInt() * 3 / 2, planar.size)

                        val packed = frame.pixels(VideoPixelFormat.RGBA)
                        assertEquals(frame.width().toInt() * frame.height().toInt() * 4, packed.size)
                        assertTrue(packed.indices.filter { it % 4 == 3 }.all { packed[it] == 0xFF.toByte() })
                    }

                    video.finish()
                }
            }
        }
    }

    @Test
    fun `encode audio with opus object`() {
        val input = AudioEncoderInput(format = AudioSampleFormat.F32, sampleRate = 48_000u, channels = 1u)
        val silence = AudioFrame(timestampUs = 0uL, data = ByteArray(960 * 4))

        // The producer retains the codec, so releasing the codec and the
        // config in either order must still encode.
        for (codecFirst in listOf(true, false)) {
            BroadcastProducer().use { broadcast ->
                val codec = AudioCodec.opus()
                val output = AudioEncoderOutput(codec = codec)
                val producer = AudioProducer(
                    CatalogProducer(broadcast),
                    AudioProducerOptions(
                        input = input,
                        output = output,
                        track = "mic",
                    ),
                )
                if (codecFirst) {
                    codec.close()
                } else {
                    output.destroy()
                }
                producer.write(silence)
                if (codecFirst) {
                    output.destroy()
                } else {
                    codec.close()
                }
                assertEquals("mic", producer.demand().name())
                producer.finish()
            }
        }
    }
}

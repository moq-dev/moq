package dev.moq

import dev.moq.audio.Consumer as AudioConsumer
import dev.moq.audio.Producer as AudioProducer
import dev.moq.audio.ProducerOptions as AudioProducerOptions
import dev.moq.audio.ReadFrame as AudioReadFrame
import dev.moq.json.SnapshotConfig
import dev.moq.json.SnapshotConsumer
import dev.moq.json.SnapshotProducer
import dev.moq.json.StreamConsumer
import dev.moq.json.StreamProducer
import dev.moq.json.Timed
import dev.moq.json.WindowConfig
import dev.moq.json.WindowConsumer
import dev.moq.json.WindowEventPop
import dev.moq.json.WindowEventPush
import dev.moq.json.WindowProducer
import dev.moq.json.decode
import dev.moq.json.events
import dev.moq.json.push
import dev.moq.json.update
import dev.moq.json.valuesAs
import dev.moq.media.*
import dev.moq.video.Consumer as VideoConsumer
import dev.moq.video.FrameRate as VideoFrameRate
import dev.moq.video.Producer as VideoProducer
import dev.moq.video.ProducerOptions as VideoProducerOptions
import dev.moq.video.ReadFrame as VideoReadFrame
import java.net.DatagramSocket
import java.net.InetAddress
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNotEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlin.time.Duration
import kotlin.time.Duration.Companion.days
import kotlin.time.Duration.Companion.milliseconds
import kotlin.time.Duration.Companion.nanoseconds
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.toList
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.Serializable
import uniffi.moq.MoqException

@Serializable private data class Status(val state: String)

/** The next announce event. */
private suspend fun AnnounceConsumer.nextRoute(): AnnounceEvent =
    checkNotNull(next()) { "announce stream ended" }

private fun opusHead(): ByteArray =
    "OpusHead".encodeToByteArray() +
        byteArrayOf(
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
    fun `datagram-only requests have independent priority demand`() = runTest {
        BroadcastProducer().use { broadcast ->
            val stream = broadcast.publishTrack("events", null)
            val requests = broadcast.requests().datagrams()
            val consumer = broadcast.consume().datagrams("events")
            consumer.setPriority(12)
            val request = requests.requested()
            assertEquals("events", request.name())
            assertEquals(12.toByte(), request.priority())
            val producer = request.accept(null)
            assertEquals(12.toByte(), producer.priority())
            assertFalse(stream.demand().wanted())
            producer.insert(Datagram(7uL, Timestamp.unscaled(7uL), "seven".encodeToByteArray()))
            val datagram = assertNotNull(consumer.recv())
            assertEquals(7uL, datagram.sequence)
            assertEquals("seven", datagram.payload.decodeToString())
            // An unscaled append writes at the sequence it names.
            assertEquals(9uL, producer.append(Timestamp.unscaled(9uL), "nine".encodeToByteArray()))
            assertEquals(Timestamp.unscaled(9uL), assertNotNull(consumer.recv()).timestamp)
            consumer.cancel()
            assertNull(producer.priorityChanged(12))
            producer.finish()
            requests.cancel()
        }
    }

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
                        ContainerProducer(
                            other,
                            ContainerInit(ContainerFormat.fmp4(), byteArrayOf(), timebase),
                        )
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
            val producer = track.appendGroup(Frame(ByteArray(0), Timestamp.unscaled(0uL)))
            val group = broadcast.consume().fetchGroup("errors", 0uL, FetchGroupOptions())
            producer.abort(404u)
            val error = assertFailsWith<MoqException.Protocol> { group.readFrame() }
            assertEquals(ErrorScope.STREAM, error.details.scope)
            assertEquals(468u, error.details.code)
            assertEquals(ProtocolKind.APP, error.details.kind)
        }
    }

    /**
     * Exercises the [Moq.connect] facade end to end without a network: a bogus dial failure reaches
     * the returned session through [Session.closed]. Also proves the native lib loads through the
     * transitive `moq-ffi` dependency. One-shot mode, since the default reconnect would retry the
     * dial with backoff instead of failing.
     */
    @Test
    fun `connect fails fast and surfaces a MoqException`() = runTest {
        val ex =
            assertFailsWith<MoqException> {
                Moq.connect(
                        "https://localhost:0/test",
                        ClientConfig(tls = ClientTls(insecure = true), once = true),
                    )
                    .use { moq ->
                        moq.session.closed()
                    }
            }
        assertTrue(
            ex.isShutdown || ex is MoqException.Connect || ex is MoqException.Url,
            "expected shutdown/connect/url error, got: $ex",
        )
    }

    /**
     * The WebSocket fallback knobs reach the native client: a QUIC-only dial with no head start
     * still fails fast, and an invalid value is refused up front as a config error rather than at
     * the dial.
     */
    @Test
    fun `connect accepts the websocket fallback knobs`() = runTest {
        assertFailsWith<MoqException> {
            Moq.connect(
                    "https://localhost:0/test",
                    ClientConfig(
                        tls = ClientTls(insecure = true),
                        once = true,
                        websocket =
                            WebSocketConfig(enabled = false, delay = java.time.Duration.ZERO),
                    ),
                )
                .use { moq -> moq.session.closed() }
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
        val congestion: CongestionControl = CongestionControl.bbr()
        assertEquals(congestion, QuicConfig(congestionControl = congestion).congestionControl)
        // Dialing with the reported defaults is the same as dialing with none.
        Client(defaults).close()
    }

    /**
     * The `dev.moq` typealiases resolve to the FFI objects, and the wrapper extensions apply to
     * them. Constructing through an alias is enough to confirm both at compile time + lib load at
     * runtime.
     */
    @Test
    fun `origin alias constructs and consumes`() = runTest {
        OriginProducer(OriginConfig()).use { origin ->
            origin.consume().use { /* lifecycle smoke */ }
            origin.claim("", RouteOptions()).use { /* claim smoke */ }
        }
    }

    @Test
    fun `all public ffi records and handles have aliases`() {
        val hint: VideoHint =
            VideoHint(
                coded = Dimensions(1920u, 1080u),
                bitrate = 4_000_000uL,
                framerate = 60.0,
                optimizeForLatency = true,
            )
        val snapshot: SnapshotConfig = SnapshotConfig(deltas = true)
        val properties: VideoProperties = VideoProperties(rotation = 315.0)
        val backoff: Backoff =
            Backoff(
                initial = java.time.Duration.ofMillis(500),
                multiplier = 2u,
                max = java.time.Duration.ofSeconds(10),
                timeout = java.time.Duration.ZERO,
            )
        val client: ClientConfig =
            ClientConfig(
                tls = ClientTls(insecure = true),
                quic = QuicConfig(maxStreams = 4096uL),
                websocket = WebSocketConfig(enabled = false),
                backoff = backoff,
            )
        val server: ServerConfig = ServerConfig(tls = ServerTls(generate = listOf("localhost")))
        assertEquals(4096uL, client.quic.maxStreams)
        assertEquals(listOf("localhost"), server.tls.generate)
        val status: ConnectionPhase = ConnectionPhaseConnected(version = "moq-lite-03")
        assertEquals(4_000_000uL, hint.bitrate)
        assertEquals(true, snapshot.deltas)
        assertNull(properties.display)
        assertNull(properties.flip)
        assertEquals(java.time.Duration.ofMillis(500), backoff.initial)
        assertEquals("moq-lite-03", assertIs<ConnectionPhaseConnected>(status).version)
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
    fun `a broadcast consumer's closed returns once the broadcast ends`() = runTest {
        BroadcastProducer().use { broadcast ->
            broadcast.consume().use { consumer ->
                val closed = async { consumer.closed() }
                broadcast.end()
                closed.await()
            }
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
            val track =
                broadcast.publishTrack(
                    "events",
                    TrackInfo(maxAge = java.time.Duration.ZERO, timescale = 1_000_000uL),
                )
            val group =
                track.appendGroup(
                    Frame(
                        payload = "cached".encodeToByteArray(),
                        timestamp = Timestamp(0uL, 1_000_000uL),
                    )
                )
            group.finish()

            val fetched =
                broadcast
                    .consume()
                    .fetchGroup(
                        "events",
                        0uL,
                        FetchGroupOptions(priority = 3),
                    )
            assertEquals(0uL, fetched.start())
            assertEquals("cached", fetched.readFrame()?.payload?.decodeToString())
            assertNull(fetched.readFrame())
        }
    }

    @Test
    fun `a metadata subscription reads frame headers`() = runTest {
        BroadcastProducer().use { broadcast ->
            val track =
                broadcast.publishTrack(
                    "video",
                    TrackInfo(maxAge = java.time.Duration.ZERO, timescale = 1_000_000uL),
                )
            val meta = broadcast.consume().subscribeMeta("video", null)
            assertEquals(1_000_000uL, meta.info().timescale)

            val group =
                track.appendGroup(
                    Frame(
                        payload = "key".encodeToByteArray(),
                        timestamp = Timestamp(0uL, 1_000_000uL),
                    )
                )
            group.writeFrame(Frame(payload = ByteArray(0), timestamp = Timestamp(1uL, 1_000_000uL)))
            group.finish()
            track.finish()

            val groups = meta.groupsAsArrived().map { it.start() to it.headers().toList() }.toList()
            assertEquals(
                listOf(
                    0uL to
                        listOf(
                            FrameHeader(size = 3uL, timestamp = Timestamp(0uL, 1_000_000uL)),
                            FrameHeader(size = 0uL, timestamp = Timestamp(1uL, 1_000_000uL)),
                        )
                ),
                groups,
            )
        }
    }

    @Test
    fun `a default subscription skips superseded groups at once`() {
        assertNull(Subscription.default().delay.min)
        assertEquals(java.time.Duration.ZERO, Subscription.default().delay.max)
    }

    @Test
    fun `an ascending subscription reads a cached backlog oldest first`() = runTest {
        BroadcastProducer().use { broadcast ->
            val track =
                broadcast.publishTrack(
                    "backlog",
                    TrackInfo(maxAge = null, timescale = 1_000_000uL),
                )
            for (ms in 0uL until 3uL) {
                val group =
                    track.appendGroup(
                        Frame(payload = "x".encodeToByteArray(), timestamp = Timestamp(ms, 1_000uL))
                    )
                group.finish()
            }
            val consumer =
                track.consume(
                    Subscription(
                        delay =
                            Delay(
                                min = java.time.Duration.ZERO,
                                max = java.time.Duration.ofSeconds(10),
                            ),
                        ranges =
                            listOf(Range(start = Timestamp.unscaled(0uL), order = Order.ASCENDING)),
                    )
                )
            val starts = List(3) { checkNotNull(consumer.recvGroup()).start() }
            assertEquals(listOf(0uL, 1_000uL, 2_000uL), starts)
        }
    }

    @Test
    fun `releasing a producer ends an on-demand track`() = runTest {
        BroadcastProducer().use { broadcast ->
            broadcast.requests().use { requests ->
                val idle = broadcast.publishTrack("idle", null)
                val idleDemand = idle.demand()
                while (idleDemand.wantedChanged()) {}
                idle.close()
                assertFailsWith<MoqException.Closed> { idleDemand.wantedChanged() }

                val track = broadcast.publishTrack("status", null)
                val demand = track.demand()
                while (demand.wantedChanged()) {}
                val late = broadcast.consume().subscribeTrack("status", null)
                track.close()

                val request = requests.requestedTrack()
                assertEquals("status", request.name())
                val next = request.accept(null)
                next.writeFrame(
                    Frame(
                        payload = "served again".encodeToByteArray(),
                        timestamp = Timestamp.unscaled(0uL),
                    )
                )
                assertEquals("served again", late.readFrame()?.payload?.decodeToString())
                late.cancel()
            }
        }
    }

    @Test
    fun `raw writes on a scale-less track carry the group key`() = runTest {
        BroadcastProducer().use { broadcast ->
            val track = broadcast.publishTrack("untimed", null)
            val consumer = track.consume(null)
            track.writeFrame(
                Frame(payload = "payload".encodeToByteArray(), timestamp = Timestamp.unscaled(0uL))
            )
            val frame = consumer.readFrame()
            assertEquals("payload", frame?.payload?.decodeToString())
            assertEquals(Timestamp.unscaled(0uL), frame?.timestamp)
            assertNull(frame?.timestamp?.scale)
            assertNull(consumer.info().timescale)
            track.finish()
        }
    }

    @Test
    fun `an omitted info has a zero max age and a null one sets none`() {
        BroadcastProducer().use { broadcast ->
            val omitted = broadcast.publishTrack("omitted", null).consume(null).info()
            assertEquals(java.time.Duration.ZERO, omitted.maxAge)
            val unlimited =
                broadcast.publishTrack("unlimited", TrackInfo(maxAge = null)).consume(null).info()
            assertNull(unlimited.maxAge)
        }
    }

    @Test
    fun `a refresh reads back as published and a bare one asks for nothing`() {
        BroadcastProducer().use { broadcast ->
            val refresh = TrackRefresh(ratio = 2u, interval = java.time.Duration.ofMillis(500))
            val info = TrackInfo(maxAge = java.time.Duration.ZERO, refresh = refresh)
            assertEquals(
                refresh,
                broadcast.publishTrack("refresh", info).consume(null).info().refresh,
            )
            assertEquals(
                TrackRefresh(),
                broadcast.publishTrack("plain", null).consume(null).info().refresh,
            )
        }
    }

    @Test
    fun `refresh returns once the first subscriber asks for a group`() = runTest {
        BroadcastProducer().use { broadcast ->
            val info =
                TrackInfo(maxAge = java.time.Duration.ZERO, refresh = TrackRefresh(ratio = 0u))
            val track = broadcast.publishTrack("status", info)
            val demand = track.demand()
            track.consume(null).use { consumer ->
                demand.refresh()
                // A track with no group has nothing more to send: its first is already asked for.
                consumer.refresh()
            }
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

    /**
     * Updates published before the reader joins reach a holding reader in order, each with its
     * timestamp; an arrival reader takes only the newest.
     */
    @Test
    fun `a holding catalog reader reads every update within its delay`() = runTest {
        BroadcastProducer().use { broadcast ->
            val catalog = CatalogProducer(broadcast)
            for (value in listOf("1", "2")) catalog.setSection("app", value)
            val consumer = broadcast.consume()

            for ((delay, expected) in
                listOf(
                    java.time.Duration.ZERO to listOf("2"),
                    java.time.Duration.ofMinutes(1) to listOf("1", "2"),
                )) {
                val reader = CatalogConsumer.subscribe(consumer, delay)
                val read = mutableListOf<String>()
                var last = 0uL
                while (read.lastOrNull() != "2") {
                    val update = assertNotNull(reader.next(), "$delay delay")
                    assertNotNull(update.timestamp.scale)
                    assertTrue(update.timestamp.value > last, "$delay delay")
                    last = update.timestamp.value
                    // The broadcast's first catalog, published before any section, is not ours.
                    update.sections["app"]?.let { read.add(it) }
                }
                reader.cancel()
                assertEquals(expected, read, "$delay delay")
            }
        }
    }

    /** A fetched media group streams its decoded frames and then completes. */
    @Test
    fun `media group helper streams fetched frames`() = runTest {
        BroadcastProducer().use { broadcast ->
            val media =
                dev.moq.media.TrackProducer.audio(
                    broadcast,
                    Named(null),
                    AudioInit(format = AudioFormat.opus(), data = opusHead()),
                )
            val consumer = broadcast.consume()
            val (name, audio) = catalog(consumer).audio.entries.single()

            media.writeFrame(
                Frame(
                    payload = "opus frame".encodeToByteArray(),
                    timestamp = 5.seconds.toTimestamp(),
                )
            )

            // Fetch while the track is still published: finishing the media producer
            // unpublishes it, and the fetch would then miss with NotFound. A group is named by
            // its start, its first packet's timestamp in microseconds.
            val fetched =
                dev.moq.media.GroupConsumer.fetch(
                    consumer,
                    dev.moq.media.GroupConfig(
                        rendition = dev.moq.media.AudioRendition(name, audio),
                        start = 5_000_000uL,
                        options = FetchGroupOptions(priority = 3),
                    ),
                )

            // Close the group so the fetched stream terminates instead of waiting for more.
            media.finish()

            fetched.use {
                assertEquals(5_000_000uL, it.start())
                val frames = it.frames().toList()
                assertEquals(1, frames.size)
                val frame = frames.single()
                assertEquals("opus frame", frame.payload.decodeToString())
                assertEquals(5.seconds, frame.timestamp.toDuration())
            }
        }
    }

    /** The typed JSON helpers round-trip a `@Serializable` value. */
    /**
     * Video has the same Flow audio does. Decoding real frames needs an encoder backend, so this
     * pins the extension's shape; the decode itself is covered by the interop test (`just test
     * interop`).
     */
    @Test
    fun `video consumer exposes a reads flow like audio does`() {
        val video: (dev.moq.video.Consumer) -> Flow<dev.moq.video.Read> =
            dev.moq.video.Consumer::reads
        val audio: (dev.moq.audio.Consumer) -> Flow<dev.moq.audio.Read> =
            dev.moq.audio.Consumer::reads

        assertNotNull(video)
        assertNotNull(audio)
    }

    /** A 90 kHz tick, a present zero, and an unscaled key survive a write and read exactly. */
    @Test
    fun `timestamps round-trip exactly`() = runTest {
        for (timestamp in
            listOf(Timestamp(1uL, 90_000uL), Timestamp(0uL, 90_000uL), Timestamp.unscaled(0uL))) {
            BroadcastProducer().use { broadcast ->
                // A timed track refuses an unscaled frame, so it goes to a scale-less one.
                val track =
                    broadcast.publishTrack(
                        "ticks",
                        timestamp.scale?.let {
                            TrackInfo(maxAge = java.time.Duration.ZERO, timescale = it)
                        },
                    )
                val consumer = track.consume(null)
                track.writeFrame(Frame(payload = ByteArray(0), timestamp = timestamp))
                assertEquals(timestamp, consumer.readFrame()?.timestamp)
            }
        }
    }

    /** A zero scale, a scale past 2^62 - 1, and a value past 2^61 - 1 are refused at the write. */
    @Test
    fun `out of range timestamps are refused`() {
        BroadcastProducer().use { broadcast ->
            val track =
                broadcast.publishTrack(
                    "ticks",
                    TrackInfo(maxAge = java.time.Duration.ZERO, timescale = 90_000uL),
                )
            for (timestamp in
                listOf(
                    Timestamp(1uL, 0uL),
                    Timestamp(1uL, 1uL shl 62),
                    Timestamp(1uL shl 61, 90_000uL),
                )) {
                assertFailsWith<MoqException.TimeOverflow> {
                    track.writeFrame(Frame(payload = ByteArray(0), timestamp = timestamp))
                }
            }
        }
    }

    /**
     * From a duration is exact at nanosecond scale; to a duration rounds down to a whole
     * nanosecond.
     */
    @Test
    fun `timestamps convert to and from durations`() {
        assertEquals(Timestamp(20_000_000uL, 1_000_000_000uL), 20.milliseconds.toTimestamp())
        assertFailsWith<IllegalArgumentException> { (-1).nanoseconds.toTimestamp() }
        assertEquals(11_111.nanoseconds, Timestamp(1uL, 90_000uL).toDuration())
        assertFailsWith<IllegalArgumentException> { Timestamp.unscaled(1uL).toDuration() }
        assertEquals(666_666_666.nanoseconds, Timestamp(2uL, 3uL).toDuration())
        assertEquals(0.nanoseconds, Timestamp(1uL, 2_000_000_000uL).toDuration())
        // The sub-second remainder times a billion overflows 64 bits at the largest scale, and the
        // result is just under half a second.
        assertEquals(
            499_999_999.nanoseconds,
            Timestamp((1uL shl 61) - 1uL, (1uL shl 62) - 1uL).toDuration(),
        )
        assertFailsWith<IllegalArgumentException> {
            Timestamp((1uL shl 61) - 1uL, 1uL).toDuration()
        }
        // A quotient past a Long fails rather than wrapping to a negative duration.
        assertFailsWith<IllegalArgumentException> { Timestamp(ULong.MAX_VALUE, 1uL).toDuration() }
        assertFailsWith<IllegalArgumentException> { Timestamp(1uL, 0uL).toDuration() }
        // Past what `inWholeNanoseconds` holds, the duration fails rather than saturating.
        assertFailsWith<IllegalArgumentException> { Duration.INFINITE.toTimestamp() }
        assertFailsWith<IllegalArgumentException> { (600 * 365).days.toTimestamp() }
        assertEquals(
            Timestamp(400uL * 365uL * 86_400uL * 1_000_000_000uL, 1_000_000_000uL),
            (400 * 365).days.toTimestamp(),
        )
        assertNull(AudioEncoderOutput(codec = AudioCodec.opus()).frameDuration)
    }

    @Test
    fun `typed json snapshot round-trips a serializable value`() = runTest {
        BroadcastProducer().use { broadcast ->
            val config = SnapshotConfig(deltas = true)
            val track =
                broadcast.publishTrack(
                    "status",
                    TrackInfo(maxAge = java.time.Duration.ZERO, timescale = 1_000_000uL),
                )
            SnapshotProducer(broadcast, track, config).use { producer ->
                val consumer = SnapshotConsumer(broadcast.consume(), "status")
                for (n in 1..4) producer.update(
                    Status(state = "live$n"),
                    timestamp = n.milliseconds.toTimestamp(),
                )

                // `next` yields every state in order; `latest` skips the buffered third.
                assertEquals(
                    Timed(Status(state = "live1"), Timestamp(1_000uL, 1_000_000uL)),
                    consumer.next()?.decode<Status>(),
                )
                assertEquals(
                    Timed(Status(state = "live2"), Timestamp(2_000uL, 1_000_000uL)),
                    consumer.next()?.decode<Status>(),
                )
                assertEquals(
                    Timed(Status(state = "live4"), Timestamp(4_000uL, 1_000_000uL)),
                    consumer.latest()?.decode<Status>(),
                )
            }
        }
    }

    /**
     * A native encoder serves a track asked for again after its producer was released while read.
     */
    @Test
    fun `native encoders serve a renewal`() = runTest {
        BroadcastProducer().use { broadcast ->
            broadcast.requests().use { requests ->
                val catalog = CatalogProducer(broadcast)
                val mic =
                    AudioProducerOptions(
                        input =
                            AudioEncoderInput(
                                format = AudioSampleFormat.F32,
                                sampleRate = 48_000u,
                                channels = 1u,
                            ),
                        output = AudioEncoderOutput(codec = AudioCodec.opus()),
                    )
                val audio = AudioProducer(catalog, mic.copy(track = "mic"))
                var late = broadcast.consume().subscribeTrack("mic", null)
                audio.close()
                var request = requests.requestedTrack()
                assertEquals("mic", request.name())
                val served = AudioProducer.requested(catalog, request, mic)
                repeat(3) { index ->
                    served.write(
                        AudioFrame(
                            timestamp = Timestamp(index.toULong() * 960uL, 48_000uL),
                            data = ByteArray(960 * 4),
                        )
                    )
                }
                assertNotNull(late.readFrame())
                late.cancel()

                val camera =
                    VideoProducerOptions(
                        input =
                            VideoEncoderInput(
                                format = VideoPixelFormat.rgba(),
                                width = 320u,
                                height = 240u,
                                framerate = VideoFrameRate(30u, 1u),
                            ),
                        output =
                            VideoEncoderOutput(codec = VideoCodec.h264(), kind = softwareEncoder),
                    )
                val video = VideoProducer(catalog, camera.copy(track = "camera"))
                late = broadcast.consume().subscribeTrack("camera", null)
                video.close()
                request = requests.requestedTrack()
                assertEquals("camera", request.name())
                val next = VideoProducer.requested(catalog, request, camera)
                next.write(
                    VideoFrame(
                        timestamp = Timestamp(0uL, 1_000_000uL),
                        data = ByteArray(320 * 240 * 4) { 0x80.toByte() },
                    )
                )
                next.finish()
                assertNotNull(late.readFrame())
                late.cancel()
            }
        }
    }

    @Test
    fun `json release serves a reader that joined late`() = runTest {
        BroadcastProducer().use { broadcast ->
            broadcast.requests().use { requests ->
                val info = TrackInfo(maxAge = java.time.Duration.ZERO, timescale = 1_000uL)
                val producer = StreamProducer(broadcast, broadcast.publishTrack("events", info))
                val demand = producer.demand()
                while (demand.wantedChanged()) {}

                val late = StreamConsumer(broadcast.consume().subscribeTrack("events", null))
                producer.close()

                val request = requests.requestedTrack()
                assertEquals("events", request.name())
                val next = StreamProducer(broadcast, request.accept(info))
                next.append("\"served again\"", Timestamp(0uL, 1_000uL))
                assertEquals("\"served again\"", late.next()?.value)
                late.cancel()
            }
        }
    }

    @Test
    fun `json window yields pushes and pops in order`() = runTest {
        BroadcastProducer().use { broadcast ->
            val config = WindowConfig()
            val track =
                broadcast.publishTrack(
                    "log",
                    TrackInfo(
                        maxAge = java.time.Duration.ZERO,
                        timescale = 1_000_000uL,
                        compression = Compression.deflate(),
                    ),
                )
            val producer = WindowProducer(track, config)
            val consumer = WindowConsumer(broadcast.consume().subscribeTrack("log", null))
            for (n in 1..3) producer.push(
                Status(state = "live$n"),
                timestamp = Timestamp(n.toULong(), 1_000uL),
            )
            producer.pop(2uL, Timestamp(4uL, 1_000uL))

            for (n in 0..2) {
                val timed = checkNotNull(consumer.next())
                val push = assertIs<WindowEventPush>(timed.event)
                assertEquals(n.toULong(), push.index)
                assertEquals(Status(state = "live${n + 1}"), push.decode<Status>())
                assertEquals((n + 1).milliseconds, timed.timestamp.toDuration())
            }
            val pop = checkNotNull(consumer.next())
            assertEquals(WindowEventPop(0uL, 2uL), pop.event)
            assertEquals(4.milliseconds, pop.timestamp.toDuration())

            // The cut's new group restates record 2, which this reader already has.
            producer.cut()
            producer.push(Status(state = "live4"), timestamp = 5.milliseconds.toTimestamp())
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
            val track =
                broadcast.publishTrack(
                    "status",
                    TrackInfo(maxAge = java.time.Duration.ZERO, timescale = 1_000uL),
                )
            SnapshotProducer(broadcast, track, config).use { producer ->
                val demand: TrackDemand = producer.demand()
                assertEquals("status", demand.name())
                assertFalse(demand.wanted())

                val consumer = SnapshotConsumer(broadcast.consume(), "status")
                while (!demand.wantedChanged()) {}
                consumer.cancel()
                while (demand.wantedChanged()) {}
            }
        }
    }

    @Test
    fun `cancelling a flow collection cancels its native handle`() = runTest {
        BroadcastProducer().use { broadcast ->
            val config = SnapshotConfig(deltas = false)
            // A demand watcher does not own the writer; keep it alive through collection.
            val track =
                broadcast.publishTrack(
                    "status",
                    TrackInfo(maxAge = java.time.Duration.ZERO, timescale = 1_000uL),
                )
            SnapshotProducer(broadcast, track, config).use { producer ->
                val demand: TrackDemand = producer.demand()
                producer.update(Status(state = "live"), Timestamp(1uL, 1_000uL))

                val consumer = SnapshotConsumer(broadcast.consume(), "status")
                while (!demand.wantedChanged()) {}
                // `first` cancels the collection, which must release the subscription without an
                // explicit cancel.
                assertEquals(Status(state = "live"), consumer.valuesAs<Status>().first().value)
                while (demand.wantedChanged()) {}
            }
        }
    }

    /**
     * A pre-encoded `String` must reach the wire untouched: the member overload wins over the
     * reified extension, which would otherwise double-encode it into a JSON string literal.
     */
    @Test
    fun `raw json string passes through unencoded`() = runTest {
        BroadcastProducer().use { broadcast ->
            val config = SnapshotConfig(deltas = false)
            val track =
                broadcast.publishTrack(
                    "status",
                    TrackInfo(maxAge = java.time.Duration.ZERO, timescale = 1_000uL),
                )
            SnapshotProducer(broadcast, track, config).use { producer ->
                producer.update("""{"state":"raw"}""", Timestamp(1uL, 1_000uL))

                val consumer = SnapshotConsumer(broadcast.consume(), "status")
                assertEquals(
                    Timed(Status(state = "raw"), Timestamp(1uL, 1_000uL)),
                    consumer.valuesAs<Status>().first(),
                )
            }
        }
    }

    @Test
    fun `server listens, publishes, and streams requests`() = runTest {
        Server.listen(
                ServerConfig(bind = "127.0.0.1:0", tls = ServerTls(generate = listOf("localhost")))
            )
            .use { server ->
                assertTrue(server.localAddr.startsWith("127.0.0.1:"), "bound: ${server.localAddr}")

                val fingerprints = server.certFingerprints()
                assertEquals(1, fingerprints.size)
                assertEquals(64, fingerprints[0].length)

                server.route("live", RouteOptions(hidden = true)).use { route ->
                    route.announce()
                    route.withdraw()
                }
            }
    }

    @Test
    fun `a flow releases its handle however collection ends`() = runTest {
        BroadcastProducer().use { broadcast ->
            val track = broadcast.publishTrack("thrown", null)
            track.appendGroup(Frame(ByteArray(0), Timestamp.unscaled(0uL))).finish()
            // Finished, so a handle left open would read the end instead of failing.
            track.finish()
            val consumer = broadcast.consume().subscribeTrack("thrown", null)
            assertFailsWith<IllegalStateException> {
                consumer.groups().collect { error("collector failed") }
            }
            assertFailsWith<MoqException.Cancelled> { consumer.nextGroup() }
        }
    }

    @Test
    fun `ending a requests collection leaves the server listening`() = runTest {
        Server.listen(
                ServerConfig(bind = "127.0.0.1:0", tls = ServerTls(generate = listOf("localhost")))
            )
            .use { server ->
                assertNull(withTimeoutOrNull(10.milliseconds) { server.requests().first() })
                assertEquals(1, server.certFingerprints().size)
            }
    }

    @Test
    fun `sessions advertise the wrapper ahead of moq-ffi`() = runTest {
        Server.listen(
                ServerConfig(
                    bind = "127.0.0.1:0",
                    versions = listOf("moq-lite-07-wip"),
                    tls = ServerTls(generate = listOf("localhost")),
                )
            )
            .use { server ->
                Moq.connect(
                        "https://${server.localAddr}",
                        ClientConfig(
                            versions = listOf("moq-lite-07-wip"),
                            tls = ClientTls(insecure = true),
                        ),
                    )
                    .use {
                        server.requests().first().use { request ->
                            val advertised = request.peerImplementation()
                            assertTrue(
                                advertised?.startsWith("moq-kt/") == true,
                                "advertised: $advertised",
                            )
                        }
                    }
            }
    }

    @Test
    fun `viewers watch, update, and release before the session connects`() = runTest {
        Server.listen(
                ServerConfig(bind = "127.0.0.1:0", tls = ServerTls(generate = listOf("localhost")))
            )
            .use { server ->
                Moq.connect(
                        "https://${server.localAddr}",
                        ClientConfig(tls = ClientTls(insecure = true)),
                    )
                    .use { moq ->
                        val viewers: Viewers = moq.viewers()
                        // The server never accepts, so there is no receive estimate to divide.
                        val viewer: Viewer = viewers.watch(4_000_000uL)
                        viewer.use {
                            assertNull(it.rate())
                            it.update(1_000_000uL)
                            assertNull(it.rate())
                        }
                        viewers.setTarget(0uL)
                        viewers.setTarget(null)
                        // Past what a PROBE target carries: refused.
                        assertFailsWith<MoqException.OutOfRange> { viewers.watch(1uL shl 53) }
                    }
            }
    }

    @Test
    fun `closing a server releases its port`() = runTest {
        val first =
            Server.listen(
                ServerConfig(bind = "127.0.0.1:0", tls = ServerTls(generate = listOf("localhost")))
            )
        val addr = first.localAddr
        first.close()

        // No retry: close() released the listening socket before returning.
        Server.listen(ServerConfig(bind = addr, tls = ServerTls(generate = listOf("localhost"))))
            .use { rebound ->
                assertEquals(addr, rebound.localAddr)
            }
    }

    @Test
    fun `a broadcast is reachable only while announced`() = runTest {
        OriginProducer(OriginConfig()).use { origin ->
            origin.route("live", RouteOptions(hidden = true)).use { route ->
                route.broadcast().use { it.publishTrack("events", null) }
                val consumer = origin.consume()
                assertFailsWith<MoqException> { consumer.request("live") }

                route.announce()
                val announced = consumer.announced(AnnounceConfig())
                val first = assertIs<AnnounceEventPresent>(announced.nextRoute())
                assertEquals("live", first.announce.prefix)
                assertEquals(route.identity(), first.announce.route.identity)

                route.withdraw()
                val retracted = assertIs<AnnounceEventAbsent>(announced.nextRoute())
                assertEquals("live", retracted.announce.prefix)
                assertFailsWith<MoqException> { consumer.request("live") }

                // Announcing again continues the same stream.
                route.announce()
                val again = assertIs<AnnounceEventPresent>(announced.nextRoute())
                assertEquals(route.identity(), again.announce.route.identity)
                consumer.request("live")
            }
        }
    }

    @Test
    fun `closing a route retracts it while its broadcast lives on`() = runTest {
        OriginProducer(OriginConfig()).use { origin ->
            val consumer = origin.consume()
            val announced = consumer.announced(AnnounceConfig())
            val route = origin.route("live", RouteOptions())
            route.broadcast().use { broadcast ->
                assertIs<AnnounceEventPresent>(announced.nextRoute())
                consumer.request("live")

                route.close()
                assertIs<AnnounceEventAbsent>(announced.nextRoute())
                assertFailsWith<MoqException> { consumer.request("live") }
                broadcast.publishTrack("events", null)
                broadcast.end()
            }
        }
    }

    @Test
    fun `ending the broadcast retracts its route`() = runTest {
        OriginProducer(OriginConfig()).use { origin ->
            val consumer = origin.consume()
            val announced = consumer.announced(AnnounceConfig())
            origin.route("live", RouteOptions()).use { route ->
                assertIs<AnnounceEventPresent>(announced.nextRoute())
                route.broadcast().use { it.end() }
                assertIs<AnnounceEventAbsent>(announced.nextRoute())
                assertFailsWith<MoqException> { consumer.request("live") }
                assertFailsWith<MoqException.Closed> { route.announce() }
            }
        }
    }

    @Test
    fun `a stale route leaves its replacement alone`() = runTest {
        OriginProducer(OriginConfig()).use { origin ->
            origin.route("live", RouteOptions()).use { stale ->
                stale.broadcast().use { it.end() }

                origin.route("live", RouteOptions()).use { replacement ->
                    assertFailsWith<MoqException.Closed> { stale.announce() }
                    assertFailsWith<MoqException.Closed> { stale.setCost(1uL) }
                    stale.withdraw()
                    origin.consume().request("live")
                    replacement.broadcast().use { it.end() }
                }
            }
        }
    }

    @Test
    fun `a new identity restarts followers but stays listed`() = runTest {
        OriginProducer(OriginConfig()).use { origin ->
            val consumer = origin.consume()
            val announced = consumer.announced(AnnounceConfig())
            val following = consumer.follow("identity", null)
            origin.route("identity", RouteOptions()).use { first ->
                val present = assertIs<AnnounceEventPresent>(announced.nextRoute())
                assertFalse(present.announce.route.identity.isMinted())
                assertEquals(first.identity(), present.announce.route.identity)
                assertNotNull(following.available()).close()

                val camera = Identity.parse("camera")
                origin.route("identity", RouteOptions(identity = camera, epoch = 1uL)).use { second
                    ->
                    assertNotNull(following.available()).close()

                    // The takeover was not listed, so the next event is the path going absent.
                    first.withdraw()
                    second.withdraw()
                    val absent = assertIs<AnnounceEventAbsent>(announced.nextRoute())
                    assertEquals(camera, absent.announce.route.identity)
                    assertEquals("camera", absent.announce.route.identity.toString())
                }

                assertFailsWith<MoqException.InvalidIdentity> { Identity.parse("") }
            }
            following.cancel()
        }
    }

    @Test
    fun `a follower reports the path going offline and the next broadcast`() = runTest {
        OriginProducer(OriginConfig()).use { origin ->
            val following = origin.consume().follow("offline", null)
            origin.route("offline", RouteOptions()).use { route ->
                assertNotNull(following.available()).close()

                route.withdraw()
                assertNull(following.available())

                route.announce()
                assertNotNull(following.available()).close()
            }
            following.cancel()
        }
    }

    @Test
    fun `announced pattern reports captures`() = runTest {
        OriginProducer(OriginConfig()).use { origin ->
            val announced =
                origin.consume().announced(AnnounceConfig(prefix = "room", filter = "*/chat"))
            origin.route("room/alice/chat", RouteOptions()).use {
                val update = assertIs<AnnounceEventPresent>(announced.nextRoute())
                assertEquals("room/alice/chat", update.announce.prefix)
                assertEquals(listOf("alice"), update.announce.captures)
            }
        }
    }

    @Test
    fun `dynamic serves a request under a prefix`() = runTest {
        OriginProducer(OriginConfig()).use { origin ->
            origin.claim("live", RouteOptions()).use { dynamic ->
                val pending = async {
                    origin.consume().request("live/cam")
                }
                val request = assertNotNull(dynamic.requested())
                assertEquals("live/cam", request.path())
                request.route(RouteOptions(hidden = true)).use { job ->
                    assertEquals(dynamic.identity(), job.identity())
                    job.broadcast().use { fill ->
                        // Only announcing answers the requester.
                        job.announce()
                        pending.await()
                        // The route hands out one broadcast: ending it ends the route too.
                        job.broadcast().use { it.end() }
                        assertFailsWith<MoqException.Closed> { fill.publishTrack("late", null) }
                        assertFailsWith<MoqException.Closed> { job.announce() }
                    }
                }
            }
        }
    }

    @Test
    fun `raw track supports sparse groups and a known end`() {
        BroadcastProducer().use { broadcast ->
            val track = broadcast.publishTrack("sparse", null)
            track.createGroup(Frame(ByteArray(0), Timestamp.unscaled(2uL))).finish()
            track.finishAt(5uL)
            track.createGroup(Frame(ByteArray(0), Timestamp.unscaled(4uL))).finish()
            assertFailsWith<MoqException> {
                track.createGroup(Frame(ByteArray(0), Timestamp.unscaled(5uL)))
            }
            track.finish()
        }
    }

    /**
     * Opus' 2.5 ms frame reaches the encoder, and a duration outside the Opus set is refused rather
     * than silently rounded.
     */
    @Test
    fun `encode audio honors the opus frame duration set`() {
        val input =
            AudioEncoderInput(format = AudioSampleFormat.F32, sampleRate = 48_000u, channels = 1u)

        BroadcastProducer().use { broadcast ->
            val catalog = CatalogProducer(broadcast)
            val fine =
                AudioProducer(
                    catalog,
                    AudioProducerOptions(
                        input = input,
                        output =
                            AudioEncoderOutput(
                                codec = AudioCodec.opus(),
                                frameDuration = java.time.Duration.ofNanos(2_500_000),
                            ),
                        track = "fine",
                    ),
                )
            // 2.5 ms of silence at 48 kHz mono f32: exactly one encoded frame.
            fine.write(AudioFrame(timestamp = Timestamp(0uL, 48_000uL), data = ByteArray(120 * 4)))
            fine.finish()

            assertFailsWith<MoqException.Audio> {
                AudioProducer(
                    catalog,
                    AudioProducerOptions(
                        input = input,
                        output =
                            AudioEncoderOutput(
                                codec = AudioCodec.opus(),
                                frameDuration = java.time.Duration.ofMillis(2),
                            ),
                        track = "coarse",
                    ),
                )
            }
        }
    }

    /**
     * Both sides read the grant their peer gave them, and a peer that verifies no tokens of its own
     * refuses another one as unsupported.
     */
    @Test
    fun `session auth reads the grant`() = runTest {
        val version = listOf("moq-lite-07-wip")
        Server.listen(
                ServerConfig(
                    bind = "127.0.0.1:0",
                    versions = version,
                    tls = ServerTls(generate = listOf("localhost")),
                )
            )
            .use { server ->
                // Not requests().first(): ending that collection cancels the server.
                val accepted = async { server.server.accept()!!.accept() }
                val moq =
                    Moq.connect(
                        "https://${server.localAddr}",
                        ClientConfig(
                            bind = "127.0.0.1:0",
                            versions = version,
                            tls = ClientTls(insecure = true),
                            once = true,
                        ),
                    )
                moq.use {
                    val everything =
                        Grant(publish = listOf(""), subscribe = listOf(""), expires = null)
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

    /** A credential is answered by the peer, names a route and a claim, and cancels. */
    @Test
    fun `session credential lifecycle`() = runTest {
        val version = listOf("moq-lite-07-wip")
        Server.listen(
                ServerConfig(
                    bind = "127.0.0.1:0",
                    versions = version,
                    tls = ServerTls(generate = listOf("localhost")),
                )
            )
            .use { server ->
                val accepted = async { server.server.accept()!!.accept() }
                val moq =
                    Moq.connect(
                        "https://${server.localAddr}",
                        ClientConfig(
                            bind = "127.0.0.1:0",
                            versions = version,
                            tls = ClientTls(insecure = true),
                            once = true,
                        ),
                    )
                moq.use {
                    val session = accepted.await()
                    assertFailsWith<MoqException.Config> { moq.credential("") }
                    val credential: Credential = moq.credential("extra")
                    // The server verifies no tokens of its own, so it refuses this one.
                    assertFailsWith<MoqException.Unsupported> { credential.answered() }

                    val route = moq.route("room/x", RouteOptions(hidden = true))
                    val claim = moq.session.publish().claim("live", RouteOptions(hidden = true))
                    route.setAuth(credential)
                    claim.setAuth(credential)
                    route.setAuth(null)
                    claim.setAuth(null)

                    credential.cancel()
                    assertFailsWith<MoqException.Cancelled> { credential.answered() }
                    assertFailsWith<MoqException.Cancelled> { route.setAuth(credential) }
                    assertFailsWith<MoqException.Cancelled> { claim.setAuth(credential) }
                    route.cancel()
                    claim.cancel()
                    session.cancel(0u)
                }
            }
    }

    /**
     * Over moq-lite-06, whose wire carries no identity, the session mints one: passing it back pins
     * `request` and `follow`, while its text parsed is another, named identity that nothing serves.
     */
    @Test
    fun `an announced identity pins request and follow`() = runTest {
        val version = listOf("moq-lite-06")
        Server.listen(
                ServerConfig(
                    bind = "127.0.0.1:0",
                    versions = version,
                    tls = ServerTls(generate = listOf("localhost")),
                )
            )
            .use { server ->
                server.route("live", RouteOptions()).use {
                    // Not requests().first(): ending that collection cancels the server.
                    val accepted = async { server.server.accept()!!.accept() }
                    OriginProducer(OriginConfig()).use { viewer ->
                        Moq.connect(
                                "https://${server.localAddr}",
                                ClientConfig(
                                    bind = "127.0.0.1:0",
                                    versions = version,
                                    tls = ClientTls(insecure = true),
                                    consume = viewer,
                                    once = true,
                                ),
                            )
                            .use { moq ->
                                val session = accepted.await()
                                val present =
                                    assertIs<AnnounceEventPresent>(moq.announced().nextRoute())
                                val minted = present.announce.route.identity
                                assertTrue(minted.isMinted())

                                moq.request("live", minted)
                                val following = moq.follow("live", minted)
                                assertNotNull(following.available()).close()
                                following.cancel()

                                val named = Identity.parse(minted.text())
                                assertFalse(named.isMinted())
                                assertEquals(minted.text(), named.toString())
                                assertNotEquals(minted, named)
                                assertFailsWith<MoqException> { moq.request("live", named) }
                                session.cancel(0u)
                            }
                    }
                }
            }
    }

    /** The returned Session owns its pending dial even after Client cancellation. */
    @Test
    fun `session owns and cancels its first dial`() = runTest {
        // A bound socket that never answers keeps the first dial pending, so `connects`
        // cannot advance between the read below and the cancel.
        DatagramSocket(0, InetAddress.getByName("127.0.0.1")).use { silent ->
            val client =
                Client(
                    ClientConfig(
                        bind = "127.0.0.1:0",
                        tls = ClientTls(insecure = true),
                        once = true,
                    )
                )
            val session = client.connect("https://127.0.0.1:${silent.localPort}")
            client.cancel()
            assertEquals(0uL, session.state().connects)
            session.cancel(0u)
            session.closed()
            assertIs<ConnectionPhaseClosed>(session.state().phase)
        }
    }

    /**
     * A fixed playout delay is reported back as is, and an estimated one moves as frames are read.
     */
    @Test
    fun `audio consumer reports its playout delay`() = runTest {
        OriginProducer(OriginConfig()).use { origin ->
            origin.route("audio-delay", RouteOptions(hidden = true)).use { route ->
                route.broadcast().use { broadcast ->
                    val audio =
                        AudioProducer(
                            CatalogProducer(broadcast),
                            AudioProducerOptions(
                                input =
                                    AudioEncoderInput(
                                        format = AudioSampleFormat.F32,
                                        sampleRate = 48_000u,
                                        channels = 1u,
                                    ),
                                output = AudioEncoderOutput(codec = AudioCodec.opus()),
                                track = "mic",
                            ),
                        )
                    route.announce()

                    val consumer = origin.consume().request("audio-delay")
                    val catalog =
                        CatalogConsumer.subscribe(consumer, java.time.Duration.ZERO).next()!!
                    val rendition = catalog.audio["mic"]!!

                    // No max delay: a zero one skips a group once its successor is a warmup old by
                    // the native wall clock, so a read that starts late under load never returns.
                    val fixedOutput =
                        AudioDecoderOutput(
                            format = AudioSampleFormat.F32,
                            delay = Delay(min = java.time.Duration.ofMillis(80), max = null),
                            estimate = false,
                        )
                    val fixed = AudioConsumer.subscribe(consumer, "mic", rendition, fixedOutput)
                    val estimated =
                        AudioConsumer.subscribe(
                            consumer,
                            "mic",
                            rendition,
                            AudioDecoderOutput(
                                format = AudioSampleFormat.F32,
                                delay = Delay(min = java.time.Duration.ZERO, max = null),
                            ),
                        )
                    assertEquals(java.time.Duration.ofMillis(80), fixed.delay())
                    val initial = estimated.delay()

                    // Two 20 ms Opus frames of silence at 48 kHz mono.
                    for (i in 0uL until 2uL) {
                        audio.write(
                            AudioFrame(
                                timestamp = Timestamp(i * 960uL, 48_000uL),
                                data = ByteArray(960 * 4),
                            )
                        )
                    }
                    for (reader in listOf(fixed, estimated)) {
                        assertIs<AudioReadFrame>(assertNotNull(reader.next()))
                    }

                    assertEquals(java.time.Duration.ofMillis(80), fixed.delay())
                    assertTrue(
                        estimated.delay() != initial,
                        "the estimate should move as frames are read",
                    )

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
            origin.route("video-decode-frame", RouteOptions(hidden = true)).use { route ->
                route.broadcast().use { broadcast ->
                    val video =
                        VideoProducer(
                            CatalogProducer(broadcast),
                            VideoProducerOptions(
                                input =
                                    VideoEncoderInput(
                                        format = VideoPixelFormat.rgba(),
                                        width = 320u,
                                        height = 240u,
                                        framerate = VideoFrameRate(30000u, 1001u),
                                    ),
                                output =
                                    VideoEncoderOutput(
                                        codec = VideoCodec.h264(),
                                        kind = softwareEncoder,
                                    ),
                                track = "camera",
                                bandwidth = null,
                            ),
                        )
                    route.announce()

                    // Seed the track so a subscriber joining below lands on encoded media.
                    val rgba = ByteArray(320 * 240 * 4) { 0x80.toByte() }
                    video.cut()
                    for (i in 0 until 10) {
                        video.write(
                            VideoFrame(timestamp = Timestamp(i.toULong(), 30uL), data = rgba)
                        )
                    }

                    val consumer = origin.consume().request("video-decode-frame")
                    val catalog =
                        CatalogConsumer.subscribe(consumer, java.time.Duration.ZERO).next()!!
                    val rendition = catalog.video["camera"]!!

                    val decoder =
                        VideoConsumer.subscribe(consumer, "camera", rendition, VideoDecoderOutput())

                    // Keep the encoder fed so the decoder sees frames after it joined.
                    for (i in 10 until 40) {
                        video.write(
                            VideoFrame(timestamp = Timestamp(i.toULong(), 30uL), data = rgba)
                        )
                    }

                    assertIs<VideoReadFrame>(decoder.next()).frame.use { frame ->

                        // The frame outlives its consumer's cancellation.
                        decoder.cancel()
                        assertNull(frame.surface())

                        val planar = frame.pixels(VideoPixelFormat.i420())
                        assertEquals(
                            frame.width().toInt() * frame.height().toInt() * 3 / 2,
                            planar.size,
                        )

                        val packed = frame.pixels(VideoPixelFormat.rgba())
                        assertEquals(
                            frame.width().toInt() * frame.height().toInt() * 4,
                            packed.size,
                        )
                        assertTrue(
                            packed.indices
                                .filter { it % 4 == 3 }
                                .all { packed[it] == 0xFF.toByte() }
                        )
                    }

                    video.finish()
                }
            }
        }
    }

    @Test
    fun `encode audio with opus object`() {
        val input =
            AudioEncoderInput(format = AudioSampleFormat.F32, sampleRate = 48_000u, channels = 1u)
        val silence = AudioFrame(timestamp = Timestamp(0uL, 48_000uL), data = ByteArray(960 * 4))

        // The producer retains the codec, so releasing the codec and the
        // config in either order must still encode.
        for (codecFirst in listOf(true, false)) {
            BroadcastProducer().use { broadcast ->
                val codec = AudioCodec.opus()
                val output = AudioEncoderOutput(codec = codec)
                val producer =
                    AudioProducer(
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

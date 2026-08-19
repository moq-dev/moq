package dev.moq

import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.toList
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.Serializable
import uniffi.moq.MoqException
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull
import kotlin.test.assertTrue

@Serializable
private data class Status(val state: String)

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
    /**
     * Exercises the [Moq.connect] facade end to end without a network: a bogus
     * URL fails fast, and the failure surfaces as a [MoqException]. Also proves
     * the native lib loads through the transitive `moq-ffi` dependency.
     * One-shot mode, since the default reconnect would retry the dial with
     * backoff instead of failing.
     */
    @Test
    fun `connect fails fast and surfaces a MoqException`() = runTest {
        val ex = assertFailsWith<MoqException> {
            Moq.connect("https://localhost:0/test", tlsVerify = false, reconnect = false)
        }
        assertTrue(
            ex.isShutdown || ex is MoqException.Connect || ex is MoqException.Url,
            "expected shutdown/connect/url error, got: $ex",
        )
    }

    /**
     * The `dev.moq` typealiases resolve to the FFI objects, and the wrapper
     * extensions apply to them. Constructing through an alias is enough to
     * confirm both at compile time + lib load at runtime.
     */
    @Test
    fun `origin alias constructs and consumes`() = runTest {
        OriginProducer(OriginOptions()).use { origin ->
            origin.consume().use { /* lifecycle smoke */ }
            origin.dynamic().use { /* dynamic origin smoke */ }
        }
    }

    @Test
    fun `all public ffi records and handles have aliases`() {
        val hint: VideoHint = VideoHint(
            coded = Dimensions(1920u, 1080u),
            displayAspect = null,
            bitrate = 4_000_000uL,
            framerate = 60.0,
            optimizeForLatency = true,
        )
        val snapshot: JsonSnapshotConfig = JsonSnapshotConfig(deltaRatio = 8u, compression = false)
        val stream: JsonStreamConfig = JsonStreamConfig(compression = false)
        val properties: VideoProperties = VideoProperties(rotation = 315.0)
        val backoff: Backoff = Backoff(
            initialMs = 500uL,
            multiplier = 2u,
            maxMs = 10_000uL,
            timeoutMs = 0uL,
        )
        val status: ConnectionStatus = ConnectionStatus.CONNECTED
        assertEquals(4_000_000uL, hint.bitrate)
        assertEquals(8u, snapshot.deltaRatio)
        assertEquals(false, stream.compression)
        assertNull(properties.display)
        assertNull(properties.flip)
        assertEquals(500uL, backoff.initialMs)
        assertEquals(ConnectionStatus.CONNECTED, status)
    }

    @Test
    fun `broadcast updates shared video properties`() {
        BroadcastProducer().use { broadcast ->
            broadcast.setVideoProperties(VideoProperties(rotation = 315.0))
        }
    }

    @Test
    fun `broadcast consumer fetches cached group`() = runTest {
        BroadcastProducer().use { broadcast ->
            val track = broadcast.publishTrack("events", null)
            val group = track.appendGroup()
            group.writeFrame(Frame(payload = "cached".encodeToByteArray()))
            group.finish()

            val fetched = broadcast.consume().fetchGroup(
                "events",
                0uL,
                FetchGroupOptions(priority = 3u),
            )
            assertEquals(0uL, fetched.sequence())
            assertEquals("cached", fetched.readFrame()?.payload?.decodeToString())
            assertNull(fetched.readFrame())
        }
    }

    /** A fetched media group streams its decoded frames and then completes. */
    @Test
    fun `media group helper streams fetched frames`() = runTest {
        BroadcastProducer().use { broadcast ->
            val media = broadcast.publishAudio(
                AudioInit(format = AudioFormat.OPUS, data = opusHead()),
            )
            val consumer = broadcast.consume()
            val (name, audio) = consumer.catalog().audio.entries.single()

            media.writeFrame(Frame(payload = "opus frame".encodeToByteArray(), timestampUs = 5_000_000uL))

            // Fetch while the track is still published: finishing the media producer
            // unpublishes it, and the fetch would then miss with NotFound.
            val fetched: MediaGroupConsumer = consumer.fetchMediaGroup(
                name,
                0uL,
                audio.container,
                FetchGroupOptions(priority = 3u),
            )

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
    @Test
    fun `typed json snapshot round-trips a serializable value`() = runTest {
        BroadcastProducer().use { broadcast ->
            val config = JsonSnapshotConfig(deltaRatio = 0u, compression = false)
            val producer = broadcast.publishJsonSnapshot("status", config)
            producer.update(Status(state = "live"))

            val consumer = broadcast.consume().subscribeJsonSnapshot("status", config)
            assertEquals(Status(state = "live"), consumer.valuesAs<Status>().first())
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
            val config = JsonSnapshotConfig(deltaRatio = 0u, compression = false)
            val producer = broadcast.publishJsonSnapshot("status", config)
            producer.update("""{"state":"raw"}""")

            val consumer = broadcast.consume().subscribeJsonSnapshot("status", config)
            assertEquals(Status(state = "raw"), consumer.valuesAs<Status>().first())
        }
    }

    @Test
    fun `server listens, publishes, and streams requests`() = runTest {
        Server.listen("127.0.0.1:0", tlsGenerate = listOf("localhost")).use { server ->
            assertTrue(server.localAddr.startsWith("127.0.0.1:"), "bound: ${server.localAddr}")

            val fingerprints = server.certFingerprints()
            assertEquals(1, fingerprints.size)
            assertEquals(64, fingerprints[0].length)

            server.createBroadcast("live").use { broadcast ->
                broadcast.setAnnounce(false)
                broadcast.finish()
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
}

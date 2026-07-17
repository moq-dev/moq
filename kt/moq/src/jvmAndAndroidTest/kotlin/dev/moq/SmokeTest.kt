package dev.moq

import kotlinx.coroutines.flow.first
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

class SmokeTest {
    /**
     * Exercises the [Moq.connect] facade end to end without a network: a bogus
     * URL fails fast, and the failure surfaces as a [MoqException]. Also proves
     * the native lib loads through the transitive `moq-ffi` dependency.
     */
    @Test
    fun `connect fails fast and surfaces a MoqException`() = runTest {
        val ex = assertFailsWith<MoqException> {
            Moq.connect("https://localhost:0/test", tlsVerify = false)
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
        assertEquals(4_000_000uL, hint.bitrate)
        assertEquals(8u, snapshot.deltaRatio)
        assertEquals(false, stream.compression)
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
    fun `server listens, announces, and streams requests`() = runTest {
        Server.listen("127.0.0.1:0", tlsGenerate = listOf("localhost")).use { server ->
            assertTrue(server.localAddr.startsWith("127.0.0.1:"), "bound: ${server.localAddr}")

            val fingerprints = server.certFingerprints()
            assertEquals(1, fingerprints.size)
            assertEquals(64, fingerprints[0].length)

            BroadcastProducer().use { broadcast ->
                val announce = server.announce("live", broadcast)
                announce.unannounce()
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

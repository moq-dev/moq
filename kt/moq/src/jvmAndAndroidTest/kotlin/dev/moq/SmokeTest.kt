package dev.moq

import kotlinx.coroutines.async
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.toList
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.yield
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

/** Wall-clock bound on polling for a configuration race, so a regression fails instead of hanging. */
private const val CONFIG_RACE_TIMEOUT_NS = 10_000_000_000L

class SmokeTest {
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
        OriginProducer(OriginConfig()).use { origin ->
            origin.consume().use { /* lifecycle smoke */ }
            origin.dynamic("", Route()).use { /* dynamic origin smoke */ }
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
        val snapshot: JsonSnapshotConfig = JsonSnapshotConfig(deltaRatio = 8u, compression = false)
        val stream: JsonStreamConfig = JsonStreamConfig(compression = false)
        val properties: VideoProperties = VideoProperties(rotation = 315.0)
        val backoff: Backoff = Backoff(
            initialUs = 500_000uL,
            multiplier = 2u,
            maxUs = 10_000_000uL,
            timeoutUs = 0uL,
        )
        val status: ConnectionStatus = ConnectionStatus.CONNECTED
        assertEquals(4_000_000uL, hint.bitrate)
        assertEquals(8u, snapshot.deltaRatio)
        assertEquals(false, stream.compression)
        assertNull(properties.display)
        assertNull(properties.flip)
        assertEquals(500_000uL, backoff.initialUs)
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

    @Test
    fun `readFrame skips empty then populated groups`() = runTest {
        BroadcastProducer().use { broadcast ->
            val track = broadcast.publishTrack("status", null)
            val consumer = track.consume(null)
            track.appendGroup().finish()
            track.appendGroup().finish()
            track.writeFrame(Frame(payload = "populated".encodeToByteArray(), timestampUs = 2_000uL))
            val frame = consumer.readFrame()
            assertEquals("populated", frame?.payload?.decodeToString())
            assertEquals(2_000uL, frame?.timestampUs)
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
                broadcast.announce(Route())
                broadcast.unannounce()
                broadcast.finish()
            }
        }
    }

    @Test
    fun `announce then unannounce is visible`() = runTest {
        OriginProducer(OriginConfig()).use { origin ->
            origin.createBroadcast("live").use { broadcast ->
                broadcast.publishTrack("events", null)
                broadcast.announce(Route())
                val announced = origin.consume().announced(AnnounceConfig())
                val first = announced.next()!!
                assertEquals("live", first.prefix())
                assertTrue(first.active())
                broadcast.unannounce()
                val retracted = announced.next()!!
                assertEquals("live", retracted.prefix())
                assertTrue(!retracted.active())
            }
        }
    }

    @Test
    fun `announced pattern reports captures`() = runTest {
        OriginProducer(OriginConfig()).use { origin ->
            val announced = origin.consume().announced(AnnounceConfig(prefix = "room", filter = "*/chat"))
            origin.createBroadcast("room/alice/chat").use { broadcast ->
                broadcast.announce(Route())
                val update = announced.next()!!
                assertEquals("room/alice/chat", update.prefix())
                assertEquals(listOf("alice"), update.captures())
            }
        }
    }

    @Test
    fun `dynamic serves a request under a prefix`() = runTest {
        OriginProducer(OriginConfig()).use { origin ->
            origin.dynamic("live", Route()).use { dynamic ->
                val pending = async {
                    origin.consume().requestBroadcast("live/cam")
                }
                val request = dynamic.requestedBroadcast()
                assertEquals("live/cam", request.path())
                BroadcastProducer().use { served ->
                    request.accept(served)
                    pending.await()
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
     * `frameDurationUs` is microseconds so Opus' 2.5 ms frame is expressible at
     * all, and a duration outside the Opus set is refused rather than silently
     * rounded.
     */
    @Test
    fun `encode audio honors the opus frame duration set`() {
        val input = AudioEncoderInput(format = AudioSampleFormat.F32, sampleRate = 48_000u, channels = 1u)

        BroadcastProducer().use { broadcast ->
            val fine = broadcast.encodeAudio(
                "fine",
                input,
                AudioEncoderOutput(codec = AudioCodec.opus(), frameDurationUs = 2_500u),
            )
            // 2.5 ms of silence at 48 kHz mono f32: exactly one encoded frame.
            fine.write(AudioFrame(timestampUs = 0uL, data = ByteArray(120 * 4)))
            fine.finish()

            assertFailsWith<MoqException.Audio> {
                broadcast.encodeAudio(
                    "coarse",
                    input,
                    AudioEncoderOutput(codec = AudioCodec.opus(), frameDurationUs = 2_000u),
                )
            }
        }
    }

    /**
     * Configuration must apply or fail: a setter racing an in-flight connect
     * throws [MoqException.Busy], and one after [Client.cancel] throws
     * [MoqException.Cancelled]. Mirrors `test_client_setters_fail_after_cancel`
     * in `py/moq-rs/tests/test_server.py`.
     */
    @Test
    fun `client setters are busy during connect and cancelled after`() = runTest {
        Server.listen("127.0.0.1:0", tlsGenerate = listOf("localhost")).use { server ->
            val client = Client()
            client.setTlsVerify(false)
            client.setBind("127.0.0.1:0")
            // A reconnecting client would redial instead of failing the connect.
            client.setReconnect(false)

            // Nothing accepts the request, so connect parks holding the client lock.
            // runCatching, because a failed `async` would cancel the test scope
            // before `await` ever reported it.
            val connect = async { runCatching { client.connect("https://${server.localAddr}") } }

            // The lock is taken on the ffi runtime thread, so poll until it is.
            val deadline = System.nanoTime() + CONFIG_RACE_TIMEOUT_NS
            var busy: Throwable? = null
            while (busy == null && System.nanoTime() < deadline) {
                busy = runCatching { client.setTlsVerify(false) }.exceptionOrNull()
                yield()
            }
            assertTrue(busy is MoqException.Busy, "expected Busy while connecting, got: $busy")
            assertFailsWith<MoqException.Busy> { client.setBind("127.0.0.1:0") }

            client.cancel()
            assertFailsWith<MoqException.Cancelled> { client.setTlsVerify(true) }
            assertFailsWith<MoqException.Cancelled> { client.setBind("127.0.0.1:0") }

            val connected = connect.await().exceptionOrNull()
            assertTrue(connected is MoqException.Cancelled, "expected a cancelled connect, got: $connected")
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
                val producer = broadcast.encodeAudio("mic", input, output)
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
                assertEquals("mic", producer.name())
                producer.finish()
            }
        }
    }
}

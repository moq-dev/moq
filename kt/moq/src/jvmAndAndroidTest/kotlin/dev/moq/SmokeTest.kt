package dev.moq

import kotlinx.coroutines.test.runTest
import uniffi.moq.MoqException
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull
import kotlin.test.assertTrue

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
        OriginProducer().use { origin ->
            origin.consume().use { /* lifecycle smoke */ }
        }
    }

    @Test
    fun `broadcast consumer fetches cached group`() = runTest {
        BroadcastProducer().use { broadcast ->
            val track = broadcast.publishTrack("events", null)
            val group = track.appendGroup()
            group.writeFrame("cached".encodeToByteArray())
            group.finish()

            val fetched = broadcast.consume().fetchGroup(
                "events",
                0uL,
                FetchGroupOptions(priority = 3u),
            )
            assertEquals(0uL, fetched.sequence())
            assertEquals("cached", fetched.readFrame()?.decodeToString())
            assertNull(fetched.readFrame())
        }
    }
}

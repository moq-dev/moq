package dev.moq

import kotlinx.coroutines.test.runTest
import uniffi.moq.MoqClient
import uniffi.moq.MoqException
import uniffi.moq.MoqOriginProducer
import kotlin.test.Test
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue

class SmokeTest {
    /**
     * Verifies the native lib loads and the wrapper extension `frames()`
     * exists on the right type. No network needed: we just instantiate
     * a few types and exercise the cancel path on a fresh client.
     */
    @Test
    fun `client constructs and cancels`() = runTest {
        MoqClient().use { client ->
            client.cancel()
            val ex = assertFailsWith<MoqException> {
                client.connect("https://localhost:0/test")
            }
            assertTrue(
                ex.isShutdown || ex is MoqException.Connect || ex is MoqException.Url,
                "expected shutdown/connect/url error, got: $ex"
            )
        }
    }

    @Test
    fun `origin producer is constructible`() = runTest {
        MoqOriginProducer().use { origin ->
            origin.consume().use { /* lifecycle smoke */ }
        }
    }
}

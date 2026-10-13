package dev.moq.ffi

import kotlinx.coroutines.test.runTest
import uniffi.moq.MoqClient
import uniffi.moq.MoqClientConfig
import uniffi.moq.MoqException
import uniffi.moq.MoqOriginConfig
import uniffi.moq.MoqOriginProducer
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith

/**
 * Validates the native lib loads and the raw UniFFI surface is usable, with no
 * dependency on the `dev.moq` wrapper. The wrapper's own ergonomics are covered
 * by `:moq:jvmTest`.
 */
class BindingsSmokeTest {
    @Test
    fun `client constructs and connect fails fast on a bad url`() = runTest {
        MoqClient(MoqClientConfig()).use { client ->
            client.cancel()
            assertFailsWith<MoqException> {
                client.connect("https://localhost:0/test")
            }
        }
    }

    @Test
    fun `an exception prints the Rust error message`() {
        assertEquals("closed", MoqException.Closed().toString())
        assertEquals("transport: reset", MoqException.Transport("reset").toString())
    }

    @Test
    fun `origin producer constructs and consumes`() = runTest {
        MoqOriginProducer(MoqOriginConfig()).use { origin ->
            origin.consume().use { /* lifecycle smoke */ }
        }
    }
}

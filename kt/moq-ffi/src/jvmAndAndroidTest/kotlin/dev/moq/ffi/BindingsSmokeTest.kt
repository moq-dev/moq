package dev.moq.ffi

import kotlinx.coroutines.test.runTest
import uniffi.moq.MoqClient
import uniffi.moq.MoqException
import uniffi.moq.MoqOriginProducer
import kotlin.test.Test
import kotlin.test.assertFailsWith

/**
 * Validates the native lib loads and the raw UniFFI surface is usable, with no
 * dependency on the `dev.moq` wrapper. The wrapper's own ergonomics are covered
 * by `:moq:jvmTest`.
 */
class BindingsSmokeTest {
    @Test
    fun `client constructs and connect fails fast on a bad url`() = runTest {
        MoqClient().use { client ->
            client.cancel()
            assertFailsWith<MoqException> {
                client.connect("https://localhost:0/test")
            }
        }
    }

    @Test
    fun `origin producer constructs and consumes`() = runTest {
        MoqOriginProducer().use { origin ->
            origin.consume().use { /* lifecycle smoke */ }
        }
    }
}

package dev.moq

import uniffi.moq.MoqClient
import uniffi.moq.MoqSession

/**
 * Top-level entry points for the Moq protocol stack.
 */
object Moq {
    /** Connect to a MoQ relay using default client configuration. */
    suspend fun connect(url: String): MoqSession = MoqClient().use { it.connect(url) }

    /** Build a client with custom configuration before connecting. */
    fun client(): MoqClient = MoqClient()
}

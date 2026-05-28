package dev.moq

import uniffi.moq.MoqClient
import uniffi.moq.MoqClientSession

/**
 * Top-level entry points for the Moq protocol stack.
 */
object Moq {
    /**
     * Connect with a single origin attached as both publish source and
     * consume sink, the typical full-duplex client setup. Convenience over
     * `MoqClient().connectDuplex(url)`. Destructure as
     * `val (session, origin) = ...`.
     *
     * For custom TLS / bind options, build a client via [client], configure
     * it, and call `connectDuplex(url)` on it.
     */
    suspend fun connect(url: String): MoqClientSession = MoqClient().connectDuplex(url)

    /** Build a client with custom configuration before connecting.
     *  Pair with `connectDuplex(url)` for the auto-wired duplex setup,
     *  or with `connect(url)` and your own `setPublish` / `setConsume`
     *  for a custom topology. */
    fun client(): MoqClient = MoqClient()
}

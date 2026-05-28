package dev.moq

import uniffi.moq.MoqClient
import uniffi.moq.MoqClientSession
import uniffi.moq.connect as ffiConnect

/**
 * Top-level entry points for the Moq protocol stack.
 */
object Moq {
    /**
     * Connect with a single origin attached as both publish source and
     * consume sink, the typical full-duplex client setup. The returned
     * [MoqClientSession] holds both the session (for shutdown) and the
     * origin (for publishing local broadcasts and discovering remote
     * announcements). Destructure as `val (session, origin) = ...`.
     */
    suspend fun connect(url: String): MoqClientSession = ffiConnect(url)

    /** Build a client with custom configuration before connecting.
     *  Use when you need TLS / bind options or a non-duplex topology
     *  (only consume, only publish, separate origins). */
    fun client(): MoqClient = MoqClient()
}

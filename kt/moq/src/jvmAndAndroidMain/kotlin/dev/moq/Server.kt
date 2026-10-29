package dev.moq

import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.launch
import uniffi.moq.MoqException
import uniffi.moq.MoqServer

/**
 * A listening MoQ server with publish/subscribe conveniences.
 *
 * Build one with [Server.listen]. Broadcasts created via [createBroadcast] are
 * served to incoming sessions, and [requests] streams each incoming [Request]
 * for the caller to accept or reject.
 *
 * [Server] is [AutoCloseable]; `use { ... }` (or [close]) stops accepting new
 * sessions. In-flight sessions stay alive until their handles are dropped or
 * cancelled.
 */
class Server internal constructor(
    /** The underlying server handle. */
    val server: MoqServer,
    /** The bound local address, e.g. `127.0.0.1:4443`. Resolved by [listen]. */
    val localAddr: String,
    private val publishOrigin: OriginProducer?,
) : AutoCloseable {
    /**
     * Create an unannounced broadcast at [path], served to incoming sessions once announced.
     *
     * Fill `broadcast()`, then advertise it with `announce`. The broadcast's `end()`
     * ends it for good, as does releasing its last handle: the publication, each
     * `broadcast()`, and `requests()`.
     */
    fun createBroadcast(path: String): Publication {
        val origin = publishOrigin ?: throw IllegalStateException("no publish origin configured")
        return origin.createBroadcast(path)
    }

    /**
     * SHA-256 fingerprints of the configured TLS certificates, hex-encoded.
     *
     * Useful for pinning a generated self-signed certificate in a browser via
     * WebTransport's `serverCertificateHashes`.
     */
    fun certFingerprints(): List<String> = server.certFingerprints()

    /**
     * Stream of incoming sessions. Each [Request] must be answered with
     * `accept()` to complete the handshake or `reject(code)` to reject it; the
     * returned session must be held to keep the connection alive.
     *
     * The Flow completes when the server stops accepting.
     */
    fun requests(): Flow<Request> = pull({ server.cancel() }) { server.accept() }

    /**
     * Accept every session in a loop, holding each one alive in its own
     * coroutine until it closes, so memory does not grow with past connections.
     *
     * Returns when the server stops accepting. To inspect or reject requests,
     * collect [requests] instead.
     */
    suspend fun serve(): Unit = coroutineScope {
        requests().collect { request ->
            launch {
                // A session failing its handshake, or dying mid-stream, is
                // routine. Swallow it: letting it escape would cancel the scope
                // and let one client take the whole accept loop down.
                try {
                    request.accept().closed()
                } catch (e: MoqException) {
                    // Nothing to do; this session is already gone.
                }
            }
        }
    }

    /**
     * Stop accepting new sessions and release the native server handle, closing
     * the listening socket before it returns so the address can be bound again
     * immediately. In-flight sessions stay alive until their handles are dropped.
     */
    override fun close() {
        server.cancel()
    }

    companion object {
        /**
         * Bind a server described by [config] and start accepting.
         *
         * [config] carries the bind address (`[::]:443` when null), the TLS identity,
         * protocol versions, QUIC tuning, and origins. A value the native side cannot
         * use throws `MoqException.Config`.
         *
         * With neither [ServerConfig.publish] nor [ServerConfig.consume] set, one shared
         * origin is wired to both, so a broadcast announced on this server is also
         * visible to sessions publishing into it. Mirrors [Moq.connect].
         */
        suspend fun listen(config: ServerConfig): Server {
            val named = config.copy(implementation = implementation(config.implementation))
            val wired = if (named.publish == null && named.consume == null) {
                val shared = OriginProducer(OriginConfig())
                named.copy(publish = shared, consume = shared)
            } else {
                named
            }

            val server = MoqServer(wired)
            try {
                val localAddr = server.listen()
                return Server(server, localAddr, wired.publish)
            } catch (e: Throwable) {
                // listen() failed: don't leak the server handle.
                server.cancel()
                throw e
            }
        }
    }
}

package dev.moq

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.onCompletion
import kotlinx.coroutines.launch
import uniffi.moq.MoqException
import uniffi.moq.MoqOriginOptions
import uniffi.moq.MoqOriginProducer
import uniffi.moq.MoqRequest
import uniffi.moq.MoqServer

/**
 * A listening MoQ server with publish/subscribe conveniences.
 *
 * Build one with [Server.listen]. Broadcasts created via [createBroadcast] are
 * served to incoming sessions, and [requests] streams each incoming [MoqRequest]
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
    private val publishOrigin: MoqOriginProducer?,
) : AutoCloseable {
    /**
     * Create a live broadcast at [path], served to incoming sessions.
     *
     * The origin announces the path so subscribers can discover it, becoming visible
     * shortly after this returns. Toggle discoverability with `setAnnounce`; `finish()`
     * unpublishes immediately.
     */
    fun createBroadcast(path: String): BroadcastProducer {
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
     * Stream of incoming sessions. Each [MoqRequest] must be answered with
     * `accept()` to complete the handshake or `reject(code)` to reject it; the
     * returned session must be held to keep the connection alive.
     *
     * The Flow completes when the server stops accepting.
     */
    fun requests(): Flow<MoqRequest> = flow {
        while (true) {
            currentCoroutineContext().ensureActive()
            emit(server.accept() ?: break)
        }
    }.onCompletion { cause ->
        if (cause is CancellationException) server.cancel()
    }

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

    override fun close() {
        server.cancel()
    }

    companion object {
        /**
         * Bind a server at [bind] and start accepting.
         *
         * @param bind local socket address to listen on, e.g. "127.0.0.1:4443" or "[::]:443".
         * @param tlsCert PEM certificate chain paths to serve.
         * @param tlsKey PEM private key paths to serve.
         * @param tlsGenerate hostnames to generate a self-signed certificate for.
         * @param publish origin whose broadcasts are served to incoming sessions; auto-created when null.
         * @param subscribe origin that receives broadcasts published by incoming sessions; auto-created when null.
         */
        suspend fun listen(
            bind: String = "[::]:443",
            tlsCert: List<String>? = null,
            tlsKey: List<String>? = null,
            tlsGenerate: List<String>? = null,
            publish: MoqOriginProducer? = null,
            subscribe: MoqOriginProducer? = null,
        ): Server {
            // With neither side specified, wire ONE shared origin to both so a
            // broadcast announced on this server is also visible to sessions
            // publishing into it. Mirrors Moq.connect.
            val shared = if (publish == null && subscribe == null) MoqOriginProducer(MoqOriginOptions()) else null
            val publishOrigin = publish ?: shared
            val subscribeOrigin = subscribe ?: shared

            val server = MoqServer()
            try {
                server.setBind(bind)
                if (tlsCert != null) server.setTlsCert(tlsCert)
                if (tlsKey != null) server.setTlsKey(tlsKey)
                if (tlsGenerate != null) server.setTlsGenerate(tlsGenerate)
                if (publishOrigin != null) server.setPublish(publishOrigin)
                if (subscribeOrigin != null) server.setConsume(subscribeOrigin)

                val localAddr = server.listen()
                return Server(server, localAddr, publishOrigin)
            } catch (e: Throwable) {
                // listen() failed: don't leak the server handle.
                server.cancel()
                throw e
            }
        }
    }
}

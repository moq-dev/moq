package dev.moq

import kotlinx.coroutines.flow.Flow
import uniffi.moq.MoqAnnounced
import uniffi.moq.MoqAnnouncement
import uniffi.moq.MoqClient
import uniffi.moq.MoqOriginOptions
import uniffi.moq.MoqOriginProducer
import uniffi.moq.MoqSession

/**
 * A connected MoQ session with publish/subscribe conveniences.
 *
 * Build one with [Moq.connect]. The underlying [session] always exposes a
 * publisher and a subscriber (wired from the origins you pass to [connect], or
 * auto-created), so you can [announce] broadcasts and iterate [announcements]
 * without touching the raw [MoqClient] handle.
 *
 * [Moq] is [AutoCloseable]; `use { ... }` (or [close]) cancels the client,
 * which tears down the session.
 */
class Moq internal constructor(
    /** The established session. Use it for [Session.closed]/[Session.shutdown]. */
    val session: MoqSession,
    private val client: MoqClient,
) : AutoCloseable {
    /** Announce [broadcast] under [path] so subscribers can discover it. */
    fun announce(path: String, broadcast: BroadcastProducer) {
        session.publisher().announce(path, broadcast)
    }

    @Deprecated("Renamed to announce()", ReplaceWith("announce(path, broadcast)"))
    fun publish(path: String, broadcast: BroadcastProducer) {
        announce(path, broadcast)
    }

    /**
     * Discover broadcasts whose path starts with [prefix] as a [Flow]. The
     * subscription is acquired on collection and cancelled when collection
     * ends. Use [announced] for the raw handle.
     */
    fun announcements(prefix: String = ""): Flow<MoqAnnouncement> = session.consumer().announcements(prefix)

    /** Raw announcement handle under [prefix]. */
    fun announced(prefix: String = ""): MoqAnnounced = session.consumer().announced(prefix)

    override fun close() {
        client.cancel()
    }

    companion object {
        /**
         * Connect to a relay at [url] and return the live [Moq] connection.
         *
         * @param tlsVerify set false to skip certificate verification (local dev only).
         * @param bind local socket address to bind, e.g. "0.0.0.0:0".
         * @param publish origin to announce broadcasts through; auto-created when null.
         * @param subscribe origin to discover broadcasts through; auto-created when null.
         */
        suspend fun connect(
            url: String,
            tlsVerify: Boolean = true,
            bind: String? = null,
            publish: MoqOriginProducer? = null,
            subscribe: MoqOriginProducer? = null,
        ): Moq {
            // With neither side specified, wire ONE shared origin to both so a
            // broadcast published on this connection is discoverable via its own
            // announcements() (loopback). Otherwise honor what the caller passed
            // and let the FFI auto-create any side left null.
            val shared = if (publish == null && subscribe == null) MoqOriginProducer(MoqOriginOptions()) else null
            val publishOrigin = publish ?: shared
            val subscribeOrigin = subscribe ?: shared

            val client = MoqClient()
            try {
                if (!tlsVerify) client.setTlsDisableVerify(true)
                if (bind != null) client.setBind(bind)
                if (publishOrigin != null) client.setPublish(publishOrigin)
                if (subscribeOrigin != null) client.setConsume(subscribeOrigin)

                val session = client.connect(url)
                return Moq(session, client)
            } catch (e: Throwable) {
                // connect() failed: don't leak the client handle.
                client.cancel()
                throw e
            }
        }
    }
}

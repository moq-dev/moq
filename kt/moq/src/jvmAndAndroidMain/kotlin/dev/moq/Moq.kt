package dev.moq

import kotlinx.coroutines.flow.Flow
import uniffi.moq.MoqAnnounced
import uniffi.moq.MoqAnnouncedBroadcast
import uniffi.moq.MoqAnnouncement
import uniffi.moq.MoqBroadcastConsumer
import uniffi.moq.MoqClient
import uniffi.moq.MoqOriginOptions
import uniffi.moq.MoqOriginProducer
import uniffi.moq.MoqSession

/**
 * A connected MoQ session with publish/subscribe conveniences.
 *
 * Build one with [Moq.connect]. The underlying [session] always exposes a
 * publisher and a subscriber (wired from the origins you pass to [connect], or
 * auto-created), so you can [createBroadcast] and iterate [announcements]
 * without touching the raw [MoqClient] handle.
 *
 * [Moq] is [AutoCloseable]; `use { ... }` (or [close]) gracefully shuts down
 * the session and cancels the client.
 */
class Moq internal constructor(
    /** The established session. Use it for [Session.closed]/[Session.shutdown]. */
    val session: MoqSession,
    private val client: MoqClient,
) : AutoCloseable {
    /**
     * Create a live broadcast at [path] so subscribers can discover it.
     *
     * The origin announces the path, becoming visible shortly after this returns.
     * Toggle discoverability with `setAnnounce`; `finish()` unpublishes immediately.
     */
    fun createBroadcast(path: String): BroadcastProducer = session.publisher().createBroadcast(path)

    /**
     * Discover broadcasts whose path starts with [prefix] as a [Flow]. The
     * subscription is acquired on collection and cancelled when collection
     * ends. Use [announced] for the raw handle.
     */
    fun announcements(prefix: String = ""): Flow<MoqAnnouncement> = session.consumer().announcements(prefix)

    /** Raw announcement handle under [prefix]. */
    fun announced(prefix: String = ""): MoqAnnounced = session.consumer().announced(prefix)

    /**
     * Await the broadcast announced at exactly [path].
     *
     * Unlike [requestBroadcast] this waits indefinitely for a future
     * announcement. Cancel the returned handle to stop waiting.
     */
    fun announcedBroadcast(path: String): MoqAnnouncedBroadcast = session.consumer().announcedBroadcast(path)

    /**
     * Resolve the broadcast at [path] as soon as it can be served: the announced
     * broadcast if present, otherwise a dynamic fallback on the origin.
     *
     * Unlike [announcedBroadcast] this does not wait for a future announcement;
     * it throws when neither can serve the path.
     */
    suspend fun requestBroadcast(path: String): MoqBroadcastConsumer = session.consumer().requestBroadcast(path)

    override fun close() {
        session.shutdown()
        client.cancel()
    }

    companion object {
        /**
         * Connect to a relay at [url] and return the live [Moq] connection.
         *
         * @param tlsVerify set false to skip certificate verification (local dev only).
         * @param tlsRoots PEM root certificate paths to trust instead of platform roots.
         * @param tlsSystemRoots whether to also trust platform roots when custom roots are set.
         * @param tlsFingerprints peer certificate SHA-256 fingerprints to pin.
         * @param tlsCert path to a PEM certificate chain to present for mTLS.
         * @param tlsKey path to a PEM private key to present for mTLS.
         * @param bind local socket address to bind, e.g. "0.0.0.0:0".
         * @param publish origin to announce broadcasts through; auto-created when null.
         * @param subscribe origin to discover broadcasts through; auto-created when null.
         *
         * With neither [publish] nor [subscribe] given, both sides share one origin, so a
         * broadcast announced on this connection is discoverable via its own [announcements]
         * (loopback). Wiring either side opts out and isolates the two directions.
         */
        suspend fun connect(
            url: String,
            tlsVerify: Boolean = true,
            tlsRoots: List<String>? = null,
            tlsSystemRoots: Boolean? = null,
            tlsFingerprints: List<String>? = null,
            tlsCert: String? = null,
            tlsKey: String? = null,
            bind: String? = null,
            publish: MoqOriginProducer? = null,
            subscribe: MoqOriginProducer? = null,
        ): Moq {
            val client = MoqClient()
            try {
                if (!tlsVerify) client.setTlsDisableVerify(true)
                if (tlsRoots != null) client.setTlsRoots(tlsRoots)
                if (tlsSystemRoots != null) client.setTlsSystemRoots(tlsSystemRoots)
                if (tlsFingerprints != null) client.setTlsFingerprints(tlsFingerprints)
                if (tlsCert != null) client.setTlsCert(tlsCert)
                if (tlsKey != null) client.setTlsKey(tlsKey)
                if (bind != null) client.setBind(bind)
                if (publish != null) client.setPublish(publish)
                if (subscribe != null) client.setConsume(subscribe)

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

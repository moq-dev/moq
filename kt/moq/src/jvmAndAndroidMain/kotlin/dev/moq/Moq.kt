package dev.moq

/**
 * A connected MoQ session with publish/subscribe conveniences.
 *
 * Build one with [Moq.connect]. The underlying [session] always exposes a
 * publisher and a subscriber (wired from the origins in its [ClientConfig], or
 * auto-created), so you can [createBroadcast] and collect [announced] updates
 * without touching the raw [Client] handle.
 *
 * Call [shutdown] to drain finished tracks before disconnecting. [AutoCloseable]
 * disposal through `use { ... }` or [close] cancels immediately.
 */
class Moq internal constructor(
    /** The established session. Use it for [Session.closed]/[Session.shutdown]. */
    val session: Session,
    private val client: Client,
) : AutoCloseable {
    /**
     * Create an unannounced broadcast at [path], invisible to everyone until announced.
     *
     * Advertise it with `announce` after populating tracks. `end()` ends it for good; `close()`
     * (or `use`) releases the handle, which ends it once no `dynamic()` handle remains.
     */
    fun createBroadcast(path: String): BroadcastProducer = session.publish().createBroadcast(path)

    /**
     * Discover routes matching [config]; prefixes stay relative to the origin.
     * Collect `announced(config).updates()` for a [Flow] of [AnnounceEvent] that
     * cancels the handle when collection ends.
     */
    fun announced(config: AnnounceConfig = AnnounceConfig()): AnnounceConsumer =
        session.consume().announced(config)

    /**
     * Await a route covering exactly [path], then resolve the broadcast there.
     *
     * Unlike [requestBroadcast] this waits indefinitely for a future
     * announcement. Cancel the returned handle to stop waiting.
     */
    fun announcedBroadcast(path: String): AnnouncedBroadcast = session.consume().announcedBroadcast(path)

    /**
     * Resolve the broadcast at [path] as soon as it can be served: a local
     * broadcast at the exact path, the best announced route covering it, or a
     * dynamic fallback on the origin.
     *
     * Unlike [announcedBroadcast] this does not wait for a future announcement;
     * it throws when neither can serve the path.
     */
    suspend fun requestBroadcast(path: String): BroadcastConsumer = session.consume().requestBroadcast(path)

    /**
     * The connection epoch: 1 for the connect that built this session, one more on
     * each reconnect. A server-accepted session stays at 1.
     *
     * Pair it with [Session.status] to log each reconnect by number.
     */
    fun epoch(): ULong = session.epoch()

    /**
     * The session's bandwidth allocator.
     *
     * Every call returns a handle to the same registry. [Bandwidth.reserve] a
     * share for an app-owned encoder, or pass the handle to `encodeVideo` /
     * `encodeAudio`.
     */
    fun bandwidth(): Bandwidth = session.bandwidth()

    /** Drain finished tracks within one second, throwing if delivery times out. */
    suspend fun shutdown() {
        try {
            session.shutdown()
        } finally {
            client.cancel()
        }
    }

    /** Cancel immediately and release the client. */
    override fun close() {
        session.cancel(0u)
        client.cancel()
    }

    companion object {
        /**
         * Connect to a relay at [url] and return the live [Moq] connection.
         *
         * [config] carries the TLS trust, bind address, protocol versions, QUIC and
         * WebSocket tuning, reconnect pacing, and origins; every field has a default.
         * A value the native side cannot use throws `MoqException.Config`.
         *
         * With neither [ClientConfig.publish] nor [ClientConfig.consume] set, both sides
         * share one origin, so a broadcast announced on this connection is discoverable
         * via its own [announced] (loopback). Wiring either side opts out and isolates the
         * two directions.
         */
        suspend fun connect(url: String, config: ClientConfig = ClientConfig()): Moq {
            val client = Client(config)
            try {
                return Moq(client.connect(url), client)
            } catch (e: Throwable) {
                // connect() failed: don't leak the client handle.
                client.cancel()
                throw e
            }
        }
    }
}

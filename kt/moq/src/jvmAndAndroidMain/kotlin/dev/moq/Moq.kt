package dev.moq

/**
 * A connected MoQ session with publish/subscribe conveniences.
 *
 * Build one with [Moq.connect]. The underlying [session] always exposes a
 * publisher and a subscriber (wired from the origins in its [ClientConfig], or
 * auto-created), so you can [createBroadcast] and collect [announced] updates
 * without touching the raw [Client] handle.
 *
 * Call [shutdown], then await [Session.closed] to drain finished tracks before disconnecting. [AutoCloseable]
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
     * Follow exactly [path], resolving each publisher instance that serves it.
     *
     * Unlike [requestBroadcast] this waits indefinitely for a future
     * announcement, and each later `available()` waits for the next instance,
     * such as a restarted publisher. Cancel the returned handle to stop waiting.
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
     * How many times this session has connected: 1 for the connect that built it, one more on
     * each reconnect. A server-accepted session stays at 1.
     *
     * Pair it with [Session.status] to log each reconnect by number.
     */
    fun connects(): ULong = session.connects()

    /**
     * The session's bandwidth allocator.
     *
     * Every call returns a handle to the same registry. [Bandwidth.reserve] a
     * share for an app-owned encoder, or pass the handle to an audio or video
     * producer.
     */
    fun bandwidth(): Bandwidth = session.bandwidth()

    /**
     * The session's authorization: the grant its peer gave it, and more tokens to present.
     *
     * Collect [Auth.grants] to follow the grant, and [Auth.add] a fresh token before the
     * current one expires; each added token is presented again on every reconnect.
     */
    fun auth(): Auth = session.auth()

    /**
     * Request a draining close now; await `session.closed()` for its result, then [close]
     * to release the client.
     * Explicit `moq-lite-07-wip` waits for the peer to receive finished tails; older
     * Lite versions, including the default lite-06, wait only for transport ACKs.
     * IETF also waits for transport ACKs.
     */
    fun shutdown() {
        session.shutdown()
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

package dev.moq

/**
 * A MoQ session with publish/subscribe conveniences.
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
    /** The connecting or connected session. Use it for [Session.closed]/[Session.shutdown]. */
    val session: Session,
    private val client: Client,
) : AutoCloseable {
    /**
     * Create an unannounced broadcast at [path], invisible to everyone until announced.
     *
     * Fill `broadcast()`, then advertise it with `announce`. The broadcast's `end()` ends it for
     * good, as does releasing its last handle: the publication, each `broadcast()`, and `requests()`.
     */
    fun createBroadcast(path: String): Publication = session.publish().createBroadcast(path)

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
     * Unlike [request] this waits indefinitely for a future announcement, and
     * each later `available()` waits for the next instance, such as a restarted
     * publisher. Pass an [epoch] (UUIDv7 text) to follow only that instance.
     * Cancel the returned handle to stop waiting.
     */
    fun follow(path: String, epoch: String? = null): Following = session.consume().follow(path, epoch)

    /**
     * Resolve one publisher instance of the broadcast at [path] as soon as it
     * can be served: a local broadcast at the exact path, the best announced
     * route covering it, or a dynamic fallback on the origin.
     *
     * Unlike [follow] this does not wait for a future announcement; it throws
     * when neither can serve the path. Pass an [epoch] (UUIDv7 text) to refuse
     * any other instance.
     */
    suspend fun request(path: String, epoch: String? = null): BroadcastConsumer =
        session.consume().request(path, epoch)

    /**
     * The session's bandwidth allocator.
     *
     * Every call returns a handle to the same registry. [Allocator.reserve] a
     * share for an app-owned encoder, or pass the handle to an audio or video
     * producer.
     */
    fun allocator(): Allocator = session.allocator()

    /**
     * Follow the grant the peer gave this session.
     *
     * Collect `grants().updates()` for the current grant and each change. Every call returns
     * an independent consumer.
     */
    fun grants(): GrantConsumer = session.grants()

    /**
     * The credentials every dial presents ([ClientConfig.tokens]), to replace while running.
     *
     * [Tokens.set] replaces them, such as before the current ones expire: the next dial presents
     * them, and a live session with in-band AUTH presents them at once, withdrawing each
     * replaced token once its replacements are answered, even when one is refused. After an
     * authorization refusal the session redials only if they changed. An empty token throws
     * `MoqException.Config`. Suspend on [Tokens.answered] for the relay's verdict.
     */
    fun tokens(): Tokens = checkNotNull(session.tokens()) { "a client session presents tokens" }

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
         * Connect to a relay at [url] and return a connecting [Moq] connection.
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
            val client = Client(config.copy(implementation = implementation(config.implementation)))
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

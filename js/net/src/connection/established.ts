import type { Getter } from "@moq/signals";
import type * as announce from "../announced.ts";
import type * as broadcast from "../broadcast.ts";
import type * as Path from "../path.ts";
import type { Probe, Stats } from "./stats.ts";
import type { Transport } from "./transport.ts";

/** An established MoQ session, implemented by both the moq-lite and moq-ietf protocols. */
export interface Established {
	/** URL of the connected server. */
	readonly url: URL;

	/** Negotiated wire protocol version. */
	readonly version: string;

	/** The wire transport this session runs over. */
	readonly transport: Transport;

	/**
	 * Estimates measured by the peer, updated as PROBE messages arrive. Stays empty on
	 * versions without PROBE. See {@link Stats} for what the local transport counts.
	 */
	readonly probe: Getter<Probe>;

	/**
	 * Whether the relay supports broadcast discovery: announcing which broadcasts exist under a
	 * prefix. When false, {@link announced} never yields, so a consumer must subscribe blind
	 * rather than wait for an announcement. Set via `discovery` on the connect options.
	 */
	readonly discovery: boolean;

	/** Subscribe to broadcast announcements under an optional path prefix, returning paths relative to that prefix. */
	announced(prefix?: Path.Valid): announce.Consumer;

	/** Publish a broadcast at the given path. */
	publish(path: Path.Valid, broadcast: broadcast.Producer): void;

	/**
	 * Consume the broadcast at the given path, immediately.
	 *
	 * The subscription is reset if nobody publishes the path, so use
	 * {@link announcedBroadcast} instead when the broadcast may not be online yet.
	 */
	consume(path: Path.Valid): broadcast.Consumer;

	/**
	 * A reactive handle to the broadcast at the given path, live only while it is announced.
	 *
	 * The announcement-gated counterpart to {@link consume}: it waits for the broadcast to come
	 * online instead of resetting, and drops back to `undefined` when it goes away. See
	 * {@link announce.Broadcast}. Close the handle when done.
	 */
	announcedBroadcast(path: Path.Valid): announce.Broadcast;

	/**
	 * Snapshot the transport's counters, querying it fresh on each call.
	 *
	 * Resolves to an empty snapshot on a transport without `getStats()`. Sample it on
	 * whatever schedule you need rather than expecting the library to poll for you.
	 */
	stats(): Promise<Stats>;

	/** Close the session. */
	close(): void;

	/**
	 * Resolves when the session closes: `null` for a clean close, a `RemoteError` when the
	 * peer closed with a code (e.g. `SessionCode.Unauthorized` for an auth rejection), or the
	 * transport's own failure. Never rejects.
	 */
	closed: Promise<Error | null>;
}

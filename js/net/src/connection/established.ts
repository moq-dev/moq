import type { Signal } from "@moq/signals";
import type * as announce from "../announced.ts";
import type { Bandwidth } from "../bandwidth.ts";
import type * as broadcast from "../broadcast.ts";
import type * as Path from "../path.ts";
import type * as Time from "../time.ts";
import type { ConnectionStats } from "./stats.ts";
import type { Transport } from "./transport.ts";

/** An established MoQ session, implemented by both the moq-lite and moq-ietf protocols. */
export interface Established {
	/** URL of the connected server. */
	readonly url: URL;

	/** Negotiated wire protocol version. */
	readonly version: string;

	/** The wire transport this session runs over. */
	readonly transport: Transport;

	/** Estimated send bitrate from the congestion controller (if supported). */
	readonly sendBandwidth?: Bandwidth;

	/** Estimated receive bitrate from PROBE (moq-lite-03+ only). */
	readonly recvBandwidth?: Bandwidth;

	/** Smoothed RTT in milliseconds, from the transport when it measures one, otherwise from PROBE (moq-lite-04+ only). */
	readonly rtt?: Signal<Time.Milli | undefined>;

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

	/** Consume the broadcast at the given path. */
	consume(path: Path.Valid): broadcast.Consumer;

	/**
	 * Snapshot the connection's transport statistics, a cheap read of the counters
	 * in {@link ConnectionStats}. Optional so existing implementations of this
	 * interface stay source-compatible. Both built-in connections provide it.
	 */
	stats?(): Promise<ConnectionStats>;

	/** Close the session. */
	close(): void;

	/** Resolves when the session closes. */
	closed: Promise<void>;
}

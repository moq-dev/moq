import type { Getter } from "@moq/signals";
import type * as announce from "../announced.ts";
import type * as broadcast from "../broadcast.ts";
import type * as Path from "../path.ts";
import type { Stats } from "./stats.ts";
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
	 * Live transport statistics, polled from the transport and merged with the MoQ PROBE
	 * estimates. Every field is individually optional: what a connection can measure
	 * depends on the transport and the negotiated version, so read the field you need and
	 * handle `undefined` rather than assuming a populated snapshot.
	 */
	readonly stats: Getter<Stats>;

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

	/** Close the session. */
	close(): void;

	/** Resolves when the session closes. */
	closed: Promise<void>;
}

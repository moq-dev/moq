import type { Getter } from "@moq/signals";
import type * as announce from "../announced.ts";
import type * as Path from "../path.ts";
import type { Probe, Stats } from "./stats.ts";
import type { Transport } from "./transport.ts";

/**
 * An established MoQ session, implemented by both the moq-lite and moq-ietf protocols.
 *
 * Publishing goes through an origin, not the session: pass an `Origin.Consumer` as the
 * `publish` connect option and the session announces and serves that origin's broadcasts.
 */
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

	/**
	 * Subscribe to broadcast announcements matching `scope`, any pattern (`foo/**`
	 * for a subtree, `room/* /chat` for each room's chat, default `**`). Paths are
	 * relative to the session; captures report what the scope's wildcards stood for.
	 */
	announced(scope?: Path.Pattern): announce.Consumer;

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
	 * Resolves when the session closes: `null` for a clean close, an `Error.Session` when the
	 * peer closed with a code (e.g. `SessionCode.Unauthorized` for an auth rejection), or the
	 * transport's own failure. Never rejects.
	 */
	closed: Promise<Error | null>;
}

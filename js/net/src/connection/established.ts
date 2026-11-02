import type { Getter } from "@moq/signals";
import type * as announce from "../announce.ts";
import type * as Auth from "../auth.ts";
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
	 * versions without PROBE. Observe it to request updates; `peek()` alone is passive.
	 * See {@link Stats} for what the local transport counts.
	 */
	readonly probe: Getter<Probe>;

	/**
	 * The tokens this side presented and the grant they earned, plus the tokens the peer
	 * presents. On moq-lite-07-wip, and on moq-transport draft-18+ when both sides negotiate
	 * MoQ Auth, each side presents its connection's credential right after setup.
	 * Otherwise the grant stays undefined.
	 *
	 * The handle passed to `connect` as `auth`, or else one the session created for itself,
	 * which ends with it.
	 */
	readonly auth: Auth.Handle;

	/**
	 * The implementation the peer advertised in its SETUP, such as `moq-net-rs/0.3.10`.
	 *
	 * Resolves once the peer's SETUP is read: `undefined` when it advertised none, its version
	 * carries none (moq-lite 01 to 04), or the session closed first. Never rejects.
	 * Peer-controlled: use it for logs and rollout counts, never to authorize.
	 */
	readonly peerImplementation: Promise<string | undefined>;

	/**
	 * Subscribe to broadcast announcements matching `scope`, any pattern (`foo/**`
	 * for a subtree, `room/* /chat` for each room's chat, default `**`). Paths are
	 * relative to the session; captures report what the scope's wildcards stood for.
	 * Only routes whose literal name matches are reported unless `options.covering`
	 * opts in, and hidden routes unless `options.hidden` does. A peer without
	 * discovery refuses or resets the stream.
	 */
	announced(scope?: Path.Pattern, options?: announce.Options): announce.Consumer;

	/**
	 * Snapshot the transport's counters, querying it fresh on each call.
	 *
	 * Resolves to an empty snapshot on a transport without `getStats()`. Sample it on
	 * whatever schedule you need rather than expecting the library to poll for you.
	 */
	stats(): Promise<Stats>;

	/**
	 * Withdraw announcements and drain for up to one second, then close the session.
	 *
	 * Explicit lite-07-wip waits for the subscriber to receive finished tails; older
	 * Lite versions wait only for transport ACKs. IETF media streams are not drained.
	 */
	close(): Promise<void>;

	/** End the session immediately without waiting for delivery. */
	abort(): void;

	/**
	 * Resolves when the session closes: `null` for a clean close, an `Error.Session` when the
	 * peer closed with a code (e.g. `SessionCode.Unauthorized` for an auth rejection), or the
	 * transport's own failure. Never rejects.
	 */
	closed: Promise<Error | null>;
}

/**
 * GOAWAY handling for the reconnect loop: the peer's drain signal and the policy for the
 * redirect it may name. Mirrors `Goaway` and `Redirect` in `moq_tokio::connection`.
 *
 * @module
 */

import { Milli } from "@moq/clock";
import { RefusedRedirect } from "../error.ts";
import { type Endpoint, isLocal, parse, sameHost } from "./endpoint.ts";
import type { Transport } from "./transport.ts";

/** A peer's GOAWAY as a session received it. */
export interface Drain {
	/** Where to reconnect, including any credentials it needs. Empty means the same endpoint. */
	readonly uri: string;
	/** When the peer force-closes the session. Undefined when the wire carried none. */
	readonly timeout?: Milli;
}

/**
 * What to do with the URI a peer names in its GOAWAY.
 *
 * - `same-host` (the default) follows it only onto the host already dialed, so a peer can move
 *   the connection between ports or schemes but not to another host.
 * - `follow` also lets the peer name the host. The name is dialed as written, and the new host
 *   is presented every token on the connection's `auth` handle, so only use it with a relay
 *   trusted not to point the connection into the local network or at an untrusted host.
 * - `ignore` reconnects to the current URL whatever the peer names.
 *
 * A malformed or refused URI ends the connection with {@link RefusedRedirect}.
 */
export type Redirect = "follow" | "same-host" | "ignore";

/** How a reconnecting connection reacts to a peer's GOAWAY. */
export interface GoawayProps {
	/** What to do with the URI the peer names (default: `same-host`). */
	redirect?: Redirect;

	/**
	 * How long the old session keeps serving after the GOAWAY while the replacement dials
	 * (default: 10000ms). A cap: a shorter deadline from the peer wins, a longer one does not
	 * extend it.
	 */
	handover?: Milli;
}

/** The handover cap when the caller names none. */
export const DEFAULT_HANDOVER = Milli(10_000);

/**
 * How long a drained session may keep serving: the cap, lowered to the peer's deadline when
 * it named a positive one. Absence is not a zero-length handover.
 *
 * @internal
 */
export function handover(cap: Milli, timeout?: Milli): Milli {
	return timeout !== undefined && timeout > 0 ? Milli(Math.min(cap, timeout)) : cap;
}

/**
 * The URL a redirect is judged against. A WebSocket fallback that won the race is the host
 * we dialed; the primary URL was not, so same-host must not treat it as the current peer.
 *
 * @internal
 */
export function dialed(primary: URL, transport: Transport, websocket?: URL): URL {
	return transport === "websocket" && websocket ? websocket : primary;
}

/**
 * Whether a certificate pin constrains this session. The pin is a WebTransport option, so it
 * holds the host only when that transport is the one that connected.
 *
 * @internal
 */
export function pinnedTransport(transport: Transport, configured: boolean): boolean {
	return configured && transport === "webtransport";
}

/**
 * The URL a GOAWAY assigns: `undefined` keeps the current URL (the peer named none, or the
 * policy ignores a URI it could parse), and a URL replaces it. Throws {@link RefusedRedirect}
 * for an explicit URI the policy will not follow, including a malformed one under `ignore`.
 *
 * `pinned` is a certificate pin on the connection, which can only verify the host it was
 * configured for, so it refuses a host change even under `follow`.
 *
 * @internal
 */
export function target(policy: Redirect, uri: string, current: URL, pinned: boolean): URL | undefined {
	if (uri === "") return undefined;

	// The URI can carry credentials, so the error names the reason, never the URI.
	// Parse before `ignore`: a malformed redirect is terminal even when the policy
	// would otherwise stay on the current URL.
	let next: URL;
	let to: Endpoint;
	try {
		next = new URL(uri);
		to = parse(next);
	} catch {
		throw new RefusedRedirect("the GOAWAY URI is malformed");
	}
	if (policy === "ignore") return undefined;
	const from = parse(current);

	if (schemeTier(to.scheme) < schemeTier(from.scheme)) {
		throw new RefusedRedirect("the GOAWAY redirect downgrades the scheme");
	}

	// Only as far as the URL itself says: a name is dialed, never resolved here, so this
	// refuses a peer naming a local address outright, not one hiding it behind a hostname.
	// That gap is why `same-host` is the default.
	if (isLocal(to) && !isLocal(from)) {
		throw new RefusedRedirect("the GOAWAY redirect widens reachability to a local address");
	}

	// Host only, not the full authority: the port is what a peer legitimately moves us
	// across when it hands off to a sibling process on the same box.
	const same = sameHost(to, from);
	if (policy === "same-host" && !same) {
		throw new RefusedRedirect("the GOAWAY redirect leaves the current host");
	}
	if (pinned && !same) {
		throw new RefusedRedirect("the GOAWAY redirect leaves the host a certificate pin verifies");
	}

	return next;
}

/** Rank a scheme so a redirect cannot silently drop encryption; unknown schemes rank lowest. */
function schemeTier(scheme: string): number {
	switch (scheme) {
		case "https":
		case "wss":
		case "moqt":
		case "moql":
			return 2;
		case "http":
		case "ws":
		case "tcp":
			return 1;
		default:
			return 0;
	}
}

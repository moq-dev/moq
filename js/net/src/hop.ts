/**
 * Endpoint identity within a hop chain, shared by both wire protocols.
 *
 * moq-lite carries these natively on every announcement; moq-transport carries them
 * via the MoQ Cluster extension (see `ietf/cluster.ts`). Mirrors `Hop` in
 * `rs/moq-net`.
 *
 * @module
 */
import type { Duration } from "@moq/clock";
import type { Credential } from "./auth.ts";
import type * as Instance from "./instance.ts";
import { isLeadingOnes, type StreamVersion } from "./stream.ts";

/**
 * One relay's identity in a broadcast's hop chain: any 64-bit id.
 *
 * Names a *hop*, not an `Origin` routing table: this is the id a relay
 * stamps into a chain as an announcement passes through, so a receiver can spot its own
 * id and reject a loop. The SETUP parameter that carries it session-wide is `Hop` too.
 *
 * {@link toHop} validates any incoming value and brands it so the type system
 * enforces "only validated ids flow into hop chains." Internal code that synthesizes one
 * (e.g. `randomHop`) brands a raw bigint the same way.
 */
export type Hop = bigint & { readonly [hopBrand]: true };

declare const hopBrand: unique symbol;

/** Brand `value` as a {@link Hop}, refusing anything outside the unsigned 64-bit range. */
export function toHop(value: bigint): Hop {
	if (value < 0n || value >= 1n << 64n) throw new RangeError("Hop must be a non-negative 64-bit integer");
	return value as Hop;
}

/**
 * The reserved id 0, meaning "no identity".
 *
 * It stands in for an endpoint that never declared one, and any number of endpoints can
 * be 0, so it identifies nothing: it is never a loop, never a publisher two chains have
 * in common, and never excluded from an advertisement. A chain that holds a 0 anywhere
 * is anonymous for route selection.
 */
export const UNKNOWN_HOP: Hop = toHop(0n);

/**
 * The id `hop` carries on a wire in `version`'s varint form.
 *
 * A QUIC varint stops at 2^62-1, so a larger id travels as {@link UNKNOWN_HOP}: the route
 * still reaches the peer, losing loop detection through this hop on that link only.
 * Mirrors `Hop::wire_id` in rs/moq-net.
 */
export function wireHop(hop: Hop, version: StreamVersion): Hop {
	return hop >= 1n << 62n && !isLeadingOnes(version) ? UNKNOWN_HOP : hop;
}

/**
 * Maximum length of a hop chain. Must match `MAX_HOPS` in Rust's `model/origin.rs`.
 *
 * Broadcasts with longer chains are rejected, which bounds loop detection and rejects
 * pathological announcements across clusters with unbounded forwarding.
 */
export const MAX_HOPS = 32;

/**
 * Generate a fresh hop with a random non-zero id.
 *
 * `crypto.getRandomValues` is overkill for best-effort loop detection, but
 * used for slightly better distribution than `Math.random` at negligible cost.
 *
 * Older `@moq/lite` JS clients decode `AnnounceInterest.exclude_hop` as a u53
 * (number) and throw on anything > 2^53-1, so the random id stays below 2^53 to keep
 * those clients alive against fresh peers. Mirrors `Hop::random` in rs/moq-net.
 */
export function randomHop(): Hop {
	const buf = new BigUint64Array(1);
	crypto.getRandomValues(buf);
	// Mask to 53 bits.
	const raw = buf[0] & 0x1f_ffff_ffff_ffffn;
	// Guard against the (astronomically unlikely) zero draw.
	return toHop(raw === 0n ? 1n : raw);
}

/** The static cost of pulling content through a route; lower wins. */
export type Cost = bigint;

/** Common route prices. */
export const Cost = {
	/** The price a live publisher seeds. */
	zero: 0n,
};

/**
 * The stream a received route serves and what using it costs: read-only facts on an
 * announcement. Mirrors `RouteInfo` in rs/moq-net.
 *
 * Carried by an announce event. Applications never build one: an origin `Route` takes
 * `RouteOptions`, and the origin and the relays fill in the rest.
 */
export interface RouteInfo {
	/**
	 * The stream the route serves: routes with one instance serve one continuous sequence
	 * space, so a subscription resumes from one to another. Pass it to an origin `request` to
	 * pin this stream.
	 */
	readonly instance?: Instance.Valid;
	/** What pulling content via this route costs; lower wins. */
	readonly cost: Cost;
	/**
	 * Whether a usable path reaches the route's origin. An unavailable route is held: the
	 * network lost every path to it but may recover one, so the announcement stays. It ranks
	 * with a draining route, below every available one, and requests through it may fail until
	 * it recovers: request again once it turns available.
	 */
	readonly available: boolean;
	/**
	 * The round trip to the route's origin node over moq-lite-07, informational: it never ranks
	 * or selects a route.
	 *
	 * Each link on the path adds its min RTT, the lowest smoothed RTT it measured over the last
	 * 30 seconds, this session's own link included. Undefined on any other wire, on a route this
	 * origin serves itself, and while any link on the path has no measurement.
	 *
	 * A listing reports it as of the event that carries the route: a change to the RTT alone is
	 * no event.
	 */
	readonly rtt?: Duration;
}

/**
 * The read-only facts an announcement shows of `route`, leaving its epoch, hop chain,
 * credential, and drain state internal.
 *
 * @internal
 */
export function routeInfo(route: WireRoute): RouteInfo {
	return {
		instance: route.instance,
		cost: route.cost,
		available: route.available ?? true,
		rtt: route.rtt,
	};
}

/**
 * A route as the origin and sessions carry it: the {@link RouteInfo} facts plus the epoch that
 * orders an instance's incarnations, the hop chain it took, the credential that advertises it,
 * and whether it drains, which applications never see.
 *
 * @internal
 */
export interface WireRoute {
	/** See {@link RouteInfo.instance}. Always set on a route the origin delivers. */
	readonly instance?: Instance.Valid;
	/**
	 * Which incarnation of {@link instance} this is: among its routes at one prefix only the
	 * highest epoch is eligible, cheaper or not. Never compared across instances.
	 */
	readonly epoch?: bigint;
	/**
	 * The chain of hops the route has traversed, oldest first. A 0 entry is the anonymous mark;
	 * see {@link isAnonymous}. A route announced on this origin has none.
	 */
	readonly hops: readonly Hop[];
	/** What pulling content via this route costs; lower wins. */
	readonly cost: Cost;
	/**
	 * The token the request advertising this route carries, from `auth.add()`, or undefined
	 * for the session's own credential. Only the session that issued it carries it, as the
	 * token on its moq-transport PUBLISH_NAMESPACE: any other session, the inline NAMESPACE
	 * answering a solicited SUBSCRIBE_NAMESPACE, and moq-lite advertise the route unnamed.
	 * Swapping it renews the request with REQUEST_UPDATE. Local: a relay never forwards it.
	 */
	readonly auth?: Credential;
	/**
	 * The session it arrived on is going away (the peer sent a GOAWAY): it ranks below every
	 * other eligible route, whatever its cost, yet still serves when nothing else does. A
	 * preference state, never a saturated cost. Local: never forwarded, and readers never see it.
	 */
	readonly draining?: boolean;
	/** See {@link RouteInfo.available}; unset means available. */
	readonly available?: boolean;
	/**
	 * The origin node a lite-07 route reaches, which every request through it names. Unset on a
	 * route announced here and on every other version. Local: requests name it, readers never
	 * see it.
	 */
	readonly node?: bigint;
	/** See {@link RouteInfo.rtt}. */
	readonly rtt?: Duration;
}

/** Constructors and helpers for {@link WireRoute}. @internal */
export const WireRoute = {
	/** An empty hop chain at zero cost: what a publisher seeds for a live broadcast. */
	default: { hops: [], cost: Cost.zero } as WireRoute,

	/** Fill route defaults and saturate the static price at the wire ceiling. */
	normalize(route: Partial<WireRoute> = {}): WireRoute {
		const cost = route.cost ?? Cost.zero;
		if (typeof cost !== "bigint" || cost < 0n) throw new RangeError("route cost must be a non-negative bigint");
		const max = 2n ** 62n - 1n;
		return {
			instance: route.instance,
			epoch: route.instance === undefined ? undefined : (route.epoch ?? 0n),
			hops: route.hops ? [...route.hops] : [],
			// biome-ignore lint/complexity/useMathMinMax: bigint operands; Math.min/max throw on bigint.
			cost: cost > max ? max : cost,
			auth: route.auth,
			draining: route.draining,
			available: route.available,
			node: route.node,
			rtt: route.rtt,
		};
	},
};

/**
 * Whether this route passed through an anonymous hop (id 0) at any depth.
 *
 * An empty chain is a local announcement, not the anonymous mark. Ingress fills a
 * received empty list with the anonymous hop before yielding it.
 *
 * @internal
 */
export function isAnonymous(route: WireRoute): boolean {
	return route.hops.includes(UNKNOWN_HOP);
}

/**
 * Whether two routes name the same instance, epoch, hop chain, cost, auth, drain state,
 * availability, origin node, and RTT.
 *
 * @internal
 */
export function routesEqual(a: WireRoute | undefined, b: WireRoute | undefined): boolean {
	if (a === b) return true;
	if (!a || !b) return false;
	return (
		a.instance === b.instance &&
		a.epoch === b.epoch &&
		a.cost === b.cost &&
		a.auth === b.auth &&
		(a.draining ?? false) === (b.draining ?? false) &&
		(a.available ?? true) === (b.available ?? true) &&
		a.node === b.node &&
		(a.rtt === b.rtt || (!!a.rtt && !!b.rtt && a.rtt.eq(b.rtt))) &&
		a.hops.length === b.hops.length &&
		a.hops.every((hop, i) => hop === b.hops[i])
	);
}

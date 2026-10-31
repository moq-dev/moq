/**
 * Endpoint identity within a hop chain, shared by both wire protocols.
 *
 * moq-lite carries these natively on every announcement; moq-transport carries them
 * via the MoQ Cluster extension (see `ietf/cluster.ts`). Mirrors `Hop` in
 * `rs/moq-net`.
 *
 * @module
 */
import * as z from "zod/mini";
import type { Credential } from "./auth.ts";
import type * as Identity from "./identity.ts";
import { isLeadingOnes, type StreamVersion } from "./stream.ts";

/**
 * One relay's identity in a broadcast's hop chain: any 64-bit id.
 *
 * Names a *hop*, not an `Origin` routing table: this is the id a relay
 * stamps into a chain as an announcement passes through, so a receiver can spot its own
 * id and reject a loop. The SETUP parameter that carries it session-wide is `Hop` too.
 *
 * The {@link HopSchema} validates any incoming value and brands it so the type system
 * enforces "only validated ids flow into hop chains." Internal code that synthesizes one
 * (e.g. `randomHop`) uses `HopSchema.parse(...)` to brand a raw bigint.
 */
export const HopSchema = z
	.bigint()
	.check(z.refine((value) => value >= 0n && value < 1n << 64n, "Hop must be a non-negative 64-bit integer"))
	.brand("Hop");

/** A relay's hop id, validated by {@link HopSchema}. */
export type Hop = z.infer<typeof HopSchema>;

/**
 * The reserved id 0, meaning "no identity".
 *
 * It stands in for an endpoint that never declared one, and any number of endpoints can
 * be 0, so it identifies nothing: it is never a loop, never a publisher two chains have
 * in common, and never excluded from an advertisement. A chain that holds a 0 anywhere
 * is anonymous for route selection.
 */
export const UNKNOWN_HOP: Hop = HopSchema.parse(0n);

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
	return HopSchema.parse(raw === 0n ? 1n : raw);
}

/** The static cost of pulling content through a route; lower wins. */
export type Cost = bigint;

/** Common route prices. */
export const Cost = {
	/** The price a live publisher seeds. */
	zero: 0n,
};

/**
 * The path a route took through the mesh and what using it costs.
 *
 * The metadata half of an advertisement: an origin `requests()` pairs it with the
 * pattern it covers, a broadcast `announce()` with the broadcast's exact path, and
 * an announce event carries it so consumers can read it back.
 */
export interface Route {
	/**
	 * The stream the route serves: routes with one identity serve one continuous sequence
	 * space, so a subscription resumes from one to another. Changing it on a standing
	 * advertisement is a restart. Always set on a route the origin delivers; given to an
	 * announcing handle, unset keeps the identity its route already has, a fresh random one
	 * minted when the handle was made. Mirrors `Route::identity` in rs/moq-net.
	 *
	 * Set one only for a publisher that continues its group sequences across restarts and
	 * raises {@link epoch} on each: readers stitch every route of one identity together, so
	 * restarting the sequences under an old identity splices unrelated groups into their tracks.
	 */
	identity?: Identity.Valid;
	/**
	 * Which incarnation of {@link identity} this is: among its routes at one prefix only the
	 * highest epoch is eligible, cheaper or not. Never compared across identities. Ignored
	 * without an identity.
	 */
	epoch?: bigint;
	/** The chain of hops the route has traversed, oldest first. */
	hops: Hop[];
	/** What pulling content via this route costs; lower wins. */
	cost: Cost;
	/**
	 * The token the request advertising this route carries, from `auth.add()`, or undefined
	 * for the session's own credential. Only the session that issued it carries it, as the
	 * token on its moq-transport PUBLISH_NAMESPACE: any other session, the inline NAMESPACE
	 * answering a solicited SUBSCRIBE_NAMESPACE, and moq-lite advertise the route unnamed.
	 * Swapping it renews the request with REQUEST_UPDATE. Local: a relay never forwards it. Mirrors `Route::auth` in rs/moq-net.
	 */
	auth?: Credential;
}

/** Constructors and helpers for {@link Route}. */
export const Route = {
	/** An empty hop chain at zero cost: what a publisher seeds for a live broadcast. */
	default: { hops: [], cost: Cost.zero } as Route,

	/** Fill route defaults and saturate the static price at the wire ceiling. */
	normalize(
		route:
			| Route
			| { identity?: Identity.Valid; epoch?: bigint; hops?: readonly Hop[]; cost?: Cost; auth?: Credential } = {},
	): Route {
		const cost = route.cost ?? Cost.zero;
		if (typeof cost !== "bigint" || cost < 0n) throw new RangeError("route cost must be a non-negative bigint");
		const max = 2n ** 62n - 1n;
		return {
			identity: route.identity,
			epoch: route.identity === undefined ? undefined : (route.epoch ?? 0n),
			hops: route.hops ? [...route.hops] : [],
			// biome-ignore lint/complexity/useMathMinMax: bigint operands; Math.min/max throw on bigint.
			cost: cost > max ? max : cost,
			auth: route.auth,
		};
	},
};

/**
 * Whether this route passed through an anonymous hop (id 0) at any depth.
 *
 * An empty chain is a local announcement, not the anonymous mark. Ingress fills a
 * received empty list with the anonymous hop before yielding it.
 */
export function isAnonymous(route: Route): boolean {
	return route.hops.includes(UNKNOWN_HOP);
}

/** Whether two routes name the same identity, epoch, hop chain, cost, and auth. */
export function routesEqual(a: Route | undefined, b: Route | undefined): boolean {
	if (a === b) return true;
	if (!a || !b) return false;
	return (
		a.identity === b.identity &&
		a.epoch === b.epoch &&
		a.cost === b.cost &&
		a.auth === b.auth &&
		a.hops.length === b.hops.length &&
		a.hops.every((hop, i) => hop === b.hops[i])
	);
}

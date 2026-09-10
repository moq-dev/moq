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

/**
 * One relay's identity in a broadcast's hop chain, encoded as a 62-bit varint on the wire.
 *
 * Names a *hop*, not an {@link !Origin | Origin} routing table: this is the id a relay
 * stamps into a chain as an announcement passes through, so a receiver can spot its own
 * id and reject a loop. The SETUP parameter that carries it session-wide is `Hop` too.
 *
 * The {@link HopSchema} validates any incoming value and brands it so the type system
 * enforces "only validated ids flow into hop chains." Internal code that synthesizes one
 * (e.g. {@link randomHop}) uses `HopSchema.parse(...)` to brand a raw bigint.
 */
export const HopSchema = z
	.bigint()
	.check(z.refine((value) => value >= 0n && value < 1n << 62n, "Hop must be a non-negative 62-bit integer"))
	.brand("Hop");

export type Hop = z.infer<typeof HopSchema>;

/**
 * The reserved id 0, meaning "no identity".
 *
 * It stands in for an endpoint that never declared one, and any number of endpoints can
 * be 0, so it identifies nothing: it is never a loop, never a publisher two chains have
 * in common, and never excluded from an advertisement.
 */
export const UNKNOWN_HOP: Hop = HopSchema.parse(0n);

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
 * TEMPORARY: the wire format allows 62 bits, but older `@moq/lite` JS clients
 * decode `AnnounceInterest.exclude_hop` as a u53 (number) and throw on anything
 * > 2^53-1. To keep those clients alive against fresh peers, we cap the random
 * id at 53 bits. Restore to 62 bits once the u62 fix has propagated to deployed
 * bundles. Mirrors `Hop::random` in rs/moq-net.
 */
export function randomHop(): Hop {
	const buf = new BigUint64Array(1);
	crypto.getRandomValues(buf);
	// Mask to 53 bits.
	const raw = buf[0] & 0x1f_ffff_ffff_ffffn;
	// Guard against the (astronomically unlikely) zero draw.
	return HopSchema.parse(raw === 0n ? 1n : raw);
}

/**
 * What pulling content via a route costs, in two magnitudes accumulated together
 * and compared in that order: lower {@link Cost.warm} wins, and {@link Cost.cold}
 * breaks the tie.
 *
 * Both price the same path against different cache states. `warm` is what one more
 * subscription would cost the mesh right now, so it collapses to zero at any relay
 * already carrying the broadcast. `cold` prices the identical path as if nothing were
 * cached, so it keeps flowing through a warm relay unchanged and still says which of
 * two warm relays sits closer to the publisher.
 */
export interface Cost {
	/** The cost as the mesh stands today, discounted to zero at every carrying relay. */
	warm: bigint;
	/** The same path with every warm discount removed. */
	cold: bigint;
}

/** A free path in both magnitudes: what a live publisher seeds. */
export const ZERO_COST: Cost = { warm: 0n, cold: 0n };

/**
 * The path a route took through the mesh and what using it costs.
 *
 * The metadata half of an advertisement: an origin `dynamic()` pairs it with the
 * pattern it covers, a broadcast `announce()` with the broadcast's exact path, and
 * an announce event carries it so consumers can read it back.
 */
export interface Route {
	/** The chain of hops the route has traversed, oldest first. */
	hops: Hop[];
	/** What pulling content via this route costs; lower wins. */
	cost: Cost;
}

/** An empty hop chain at zero cost: what a publisher seeds for a live broadcast. */
export const DEFAULT_ROUTE: Route = { hops: [], cost: ZERO_COST };

/** Normalize a partial route, treating a bare bigint cost as both magnitudes alike. */
export function normalizeRoute(route: Route | { hops?: readonly Hop[]; cost?: Cost | bigint } = {}): Route {
	const hops = route.hops ? [...route.hops] : [];
	const cost = route.cost;
	if (cost === undefined) return { hops, cost: ZERO_COST };
	if (typeof cost === "bigint") return { hops, cost: { warm: cost, cold: cost } };
	return { hops, cost: { warm: cost.warm, cold: cost.cold } };
}

/** Whether two routes name the same hop chain and cost. */
export function routesEqual(a: Route | undefined, b: Route | undefined): boolean {
	if (a === b) return true;
	if (!a || !b) return false;
	return (
		a.cost.warm === b.cost.warm &&
		a.cost.cold === b.cost.cold &&
		a.hops.length === b.hops.length &&
		a.hops.every((hop, i) => hop === b.hops[i])
	);
}

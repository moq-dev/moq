/**
 * The MoQ Cluster extension (draft-lcurley-moq-cluster-00).
 *
 * moq-transport carries no routing information, so a peer cannot tell whether an
 * advertisement already passed through us. This extension adds it:
 *
 * - each endpoint declares its own Hop ID via the RELAY_HOPS Setup Option, which is also
 *   what negotiates the extension;
 * - every advertisement carries the HOP_PATH it traversed and the accumulated ROUTE_COST
 *   of that path, as Key-Value-Pair message parameters on PUBLISH_NAMESPACE and NAMESPACE.
 *
 * These are the semantics moq-lite carries natively on every announcement (see
 * `lite/announce.ts`); this module is the moq-transport binding, negotiated on draft-17+
 * only, where SETUP is a Key-Value-Pair block.
 *
 * We are a leaf, never a relay: the only path we can advertise is our own, so we stamp a
 * single-entry HOP_PATH and seed the cost at 0. What we get out of declaring is the other
 * direction. A relay that knows our Hop ID withholds the advertisements that already flowed
 * through us, so publishing a broadcast no longer announces it back to us, which is what
 * moq-lite has always done with its own hop chain.
 *
 * @module
 * @internal
 */

import { ProtocolViolation, reason } from "../error.ts";
import { MAX_HOPS, type Origin, OriginSchema, UNKNOWN_ORIGIN } from "../hop.ts";
import type { Reader } from "../stream.ts";
import * as Varint from "../varint.ts";
import { Parameters, SetupOption, type SetupOptions } from "./parameters.ts";
import { type IetfVersion, Version } from "./version.ts";

/**
 * Whether a version can negotiate the extension.
 *
 * Draft-17 is the first with the unified SETUP: a bare Key-Value-Pair block that a new
 * option slots into without touching the message shape. Earlier drafts exchange SETUP over
 * the legacy control stream, which this extension does not cover.
 *
 * @internal
 */
export function supported(version: IetfVersion): boolean {
	return version !== Version.DRAFT_14 && version !== Version.DRAFT_15 && version !== Version.DRAFT_16;
}

/**
 * The Hop IDs this session declared, one per direction.
 *
 * @internal
 */
export interface Hops {
	/** Our own Hop ID, declared in SETUP and stamped onto every advertisement we send. */
	self: Origin;

	/**
	 * The peer's Hop ID, or `undefined` when it declared no RELAY_HOPS.
	 *
	 * This is what decides whether advertisements carry the parameters at all, in both
	 * directions: a peer that declared nothing has not read ours either, so sending it a
	 * HOP_PATH would be a protocol violation and expecting one back would reject a legal
	 * message. A peer that declared the reserved 0 negotiated the extension but withheld its
	 * identity, so the parameters flow and there is simply nothing to exclude.
	 */
	peer?: Origin;
}

/**
 * The cluster parameters carried on one advertisement.
 *
 * Present on every PUBLISH_NAMESPACE and NAMESPACE of a session that negotiated the
 * extension, and on none of a session that did not.
 *
 * @internal
 */
export interface Advert {
	/** HOP_PATH: the path this advertisement traversed, publisher first, sender last. */
	hops: Origin[];

	/** ROUTE_COST: the marginal cost of subscribing via this advertisement. Absent on the
	 * wire means 0, so an endpoint that prices nothing sends nothing. */
	cost: bigint;
}

/**
 * Read the peer's Hop ID out of its SETUP, or `undefined` when it did not negotiate.
 *
 * @internal
 */
export function fromSetup(params: SetupOptions, version: IetfVersion): Origin | undefined {
	if (!supported(version)) return undefined;

	// RELAY_HOPS is odd, so its value is a length-prefixed byte string holding the sender's
	// Hop ID as a single varint. 0 is legal: the peer speaks the extension but withholds
	// its identity.
	const value = params.getBytes(SetupOption.RelayHops);
	if (value === undefined) return undefined;

	const [origin, rest] = Varint.decodeLeadingOnes(value);
	if (rest.length !== 0) throw new Error("trailing bytes in RELAY_HOPS");
	return OriginSchema.parse(origin);
}

/**
 * Declare our own Hop ID, which is also what negotiates the extension.
 *
 * @internal
 */
export function intoSetup(params: SetupOptions, self: Origin, version: IetfVersion) {
	if (!supported(version)) return;
	params.setBytes(SetupOption.RelayHops, Varint.encodeLeadingOnes(self));
}

/**
 * Whether the session negotiated the extension, which is what decides whether an
 * advertisement carries the parameters in either direction.
 *
 * @internal
 */
export function negotiated(ids: Hops | undefined): boolean {
	return ids?.peer !== undefined;
}

/**
 * The advertisement to send for one of our own broadcasts, or `undefined` on a session
 * that negotiated nothing.
 *
 * The sender's own Hop ID is always the last entry, so a receiver reconstructs the full
 * path without the sender restating it. We originate everything we advertise, so the path
 * is just us and the cost is 0: a subscription costs the peer nothing but the transfer,
 * since we are already producing the content.
 *
 * @internal
 */
export function advertise(ids: Hops | undefined): Advert | undefined {
	if (ids === undefined || ids.peer === undefined) return undefined;
	return { hops: [ids.self], cost: 0n };
}

/**
 * Whether this advertisement looped back through `self`.
 *
 * A receiver discards such an advertisement: forwarding it would extend the loop, and
 * subscribing through it would route the receiver back to itself. Hop ID 0 identifies
 * nothing, so a receiver that withheld its own identity detects nothing.
 *
 * @internal
 */
export function loops(advert: Advert, self: Origin): boolean {
	return self !== UNKNOWN_ORIGIN && advert.hops.includes(self);
}

/**
 * Write an advertisement's parameters, ready to encode onto the wire.
 *
 * @internal
 */
export function intoParams(advert: Advert): Parameters {
	validate(advert.hops);

	const params = new Parameters();

	// The entries fill the value, so they are written with no count of their own and framed
	// by the parameter's own length prefix.
	const hops = advert.hops.map((hop) => Varint.encodeLeadingOnes(hop));
	const value = new Uint8Array(hops.reduce((total, hop) => total + hop.length, 0));
	let offset = 0;
	for (const hop of hops) {
		value.set(hop, offset);
		offset += hop.length;
	}
	params.hopPath = value;

	// Absent means 0, so a free path sends nothing.
	if (advert.cost !== 0n) params.routeCost = advert.cost;

	return params;
}

/**
 * Read the parameters a negotiated session puts on every advertisement.
 *
 * The parameter block is mandatory there, so a message that ends before it (the base form)
 * or one whose block does not parse is the peer's violation, same as a block that parses but
 * carries no HOP_PATH.
 *
 * @internal
 */
export async function decodeParams(r: Reader, version: IetfVersion): Promise<Advert> {
	let params: Parameters;
	try {
		params = await Parameters.decode(r, version);
	} catch (err) {
		throw new ProtocolViolation(reason(err), { cause: err });
	}

	return fromParams(params);
}

/**
 * Read an advertisement out of a decoded parameter block.
 *
 * Every way this fails is one the draft says to close the session over: a negotiated session
 * that omits HOP_PATH, entries that do not exactly fill the parameter, an empty list, or a
 * non-zero Hop ID appearing twice. So they all surface as one {@link ProtocolViolation},
 * which the session dispatch acts on.
 *
 * @internal
 */
export function fromParams(params: Parameters): Advert {
	try {
		const value = params.hopPath;
		if (value === undefined) throw new Error("advertisement is missing HOP_PATH");

		const hops: Origin[] = [];
		let rest = value;
		while (rest.length > 0) {
			// A short read here means the entries did not exactly fill the length.
			const [hop, remain] = Varint.decodeLeadingOnes(rest);
			hops.push(OriginSchema.parse(hop));
			rest = remain;

			// Bail before the buffer does: a hostile length would otherwise cost us one
			// allocation per entry all the way to 64KB of parameter value.
			if (hops.length > MAX_HOPS) throw new Error(`hop count exceeds maximum ${MAX_HOPS}`);
		}

		validate(hops);
		return { hops, cost: params.routeCost ?? 0n };
	} catch (err) {
		throw new ProtocolViolation(reason(err), { cause: err });
	}
}

/**
 * Reject a path that cannot have come from a conforming sender: an empty list, one longer
 * than we accept, or a non-zero Hop ID appearing twice (a loop). Duplicate zeros are legal,
 * since 0 identifies nothing.
 */
function validate(hops: Origin[]) {
	if (hops.length === 0) throw new Error("hop path is empty");
	if (hops.length > MAX_HOPS) throw new Error(`hop count ${hops.length} exceeds maximum ${MAX_HOPS}`);

	// MAX_HOPS is 32, so the quadratic scan is cheaper than allocating a set.
	for (let i = 0; i < hops.length; i++) {
		const hop = hops[i];
		if (hop === UNKNOWN_ORIGIN) continue;
		if (hops.indexOf(hop, i + 1) !== -1) throw new Error(`hop ${hop} appears twice in the hop path`);
	}
}

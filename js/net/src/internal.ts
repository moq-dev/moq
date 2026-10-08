/**
 * Package-internal constructor hooks. Classes keep their constructors private so consumers
 * can't mint detached handles; sibling modules create instances through these hooks instead.
 * Not exported from the package entrypoint.
 *
 * @module
 */
import type { Dispose, Getter } from "@moq/signals";
import type { Consumer as BroadcastConsumer, Producer as BroadcastProducer } from "./broadcast.ts";
import type * as Epoch from "./epoch.ts";
import type { Frame, Consumer as GroupConsumer, Producer as GroupProducer } from "./group.ts";
import type { Route } from "./hop.ts";
import * as Path from "./path.ts";
import type { Timestamp } from "./time.ts";
import type { Groups, Producer, Request, Subscriber } from "./track.ts";
import type { Advertised, Advertisements } from "./wire.ts";

/** Normalize public group bounds into an inclusive start and exclusive end. */
export function groupBounds(groups: Groups = {}): { start: number; end?: number } {
	const bound = (value: Groups["start"] | undefined, start: boolean): number | undefined => {
		if (value === undefined) return undefined;
		if ((value.included === undefined) === (value.excluded === undefined)) {
			throw new Error("a group bound must be either included or excluded");
		}
		const sequence = value.included ?? value.excluded;
		if (sequence === undefined || !Number.isSafeInteger(sequence) || sequence < 0) {
			throw new Error("a group bound must be a non-negative safe integer");
		}
		return sequence + (start ? Number(value.excluded !== undefined) : Number(value.included !== undefined));
	};
	return { start: bound(groups.start, true) ?? 0, end: bound(groups.end, false) };
}

/**
 * The announce-interest prefix a scope needs on a prefix-shaped wire: its literal head.
 * The peer echoes every suffix beneath it, and the caller filters what arrives.
 */
export function scopeHead(scope: Path.Pattern): Path.Valid {
	return Path.from(scope.head);
}

/**
 * Whether a segment of `path` below `prefix` starts with `.`, which hides it from announce
 * discovery unless the request opts in. A path at or above the prefix never hides.
 */
export function hiddenBelow(prefix: Path.Valid, path: Path.Valid): boolean {
	const below = Path.stripPrefix(prefix, path);
	return below !== null && Path.parts(below).some((part) => part.startsWith("."));
}

/**
 * Where each carried route lands under the requested prefix: its suffix beneath the
 * prefix, or the empty suffix for a route above it, where the most specific such route
 * wins the way a request through the prefix would resolve.
 */
export function presented(
	prefix: Path.Valid,
	table: Advertisements,
	carries: (covered: Path.Valid) => boolean,
): Map<Path.Valid, Advertised> {
	const out = new Map<Path.Valid, Advertised>();
	let rootLen = -1;
	const requested = Path.Pattern.subtree(prefix);
	for (const [covered, candidates] of table) {
		if (!carries(covered)) continue;
		if (Path.hasPrefix(covered, prefix)) {
			if (covered.length < rootLen) continue;
			// A scoped route covers only what it claims, so the best one that can serve the prefix wins.
			const snap = candidates.find((candidate) => !candidate.claim || candidate.claim.overlaps(requested));
			if (!snap) continue;
			rootLen = covered.length;
			out.set(Path.empty(), snap);
			continue;
		}
		const suffix = Path.stripPrefix(prefix, covered);
		if (suffix !== null && candidates.length > 0) out.set(suffix, candidates[0]);
	}
	return out;
}

/** Whether the announced prefix's subtree overlaps `scope`. */
export function scopeOverlaps(scope: Path.Pattern, prefix: Path.Valid): boolean {
	return scope.overlaps(Path.Pattern.subtree(prefix));
}

/** What `scope` captures from an exact announced prefix, if it pins every wildcard. */
export function scopeCaptures(scope: Path.Pattern, prefix: Path.Valid): Path.Pattern[] | undefined {
	return scope.captures(Path.Pattern.literal(prefix));
}

/**
 * What a non-blocking group read found, which is everything the caller needs to decide what
 * to do next: no second look at the track's closed state, and no ordering rule to get wrong.
 */
export type Recv =
	/** A group to serve. It has already left the buffer, so dropping this is dropping the group. */
	| { kind: "group"; group: GroupConsumer }
	/** Nothing readable, but the track is live and may produce more. */
	| { kind: "idle" }
	/** The producer finished, yet groups above the cap are still held: raising it releases them. */
	| { kind: "boundary" }
	/** The producer finished and the buffer is drained. Nothing can follow. */
	| { kind: "done" }
	/** The track aborted. */
	| { kind: "error"; error: Error };

/** A package-internal frame read the wire publisher completes once written. */
export interface ReadGroupFrame {
	/** Frame sequence within the group. */
	sequence: number;
	/** Frame returned to the publisher. */
	frame: Frame;
	/** Mark the frame delivered or deliberately skipped by the wire publisher. */
	complete(): void;
}

/** The next group or datagram sequence shared by dynamic producers of one broadcast track. */
export interface TrackSequence {
	next: number;
}

/** Per-track sequence namespaces owned by one broadcast generation. */
export type TrackSequences = Map<string, TrackSequence>;

/** Inputs for creating a package-internal track request. */
export interface TrackRequestOptions {
	/** The requested track name. */
	name: string;
	/** The producer that will serve the request. */
	producer: Producer;
	/** Sequence namespaces shared by the broadcast generation. */
	sequences: TrackSequences;
	/** Requests not yet accepted or rejected by the publisher. */
	pending: Set<Request>;
}

/** Hooks assigned in static blocks by the owning class. */
export const hooks: {
	/** Mint a track {@link Request}; assigned by `track.ts`. */
	makeRequest: (options: TrackRequestOptions) => Request;
	/** Access the existing producer while a request awaits immutable wire metadata. */
	pendingTrackProducer: (request: Request) => Producer;
	/**
	 * Take the next group the subscriber's cursor allows, without waiting; assigned by `track.ts`.
	 *
	 * Synchronous so a caller can pop a group and act on it in the same turn. Park on
	 * {@link groupChanged} when it reports `idle` or `boundary`.
	 */
	tryRecvGroup: (subscriber: Subscriber) => Recv;
	/** Wake once a subscriber's group cursor may read differently; assigned by `track.ts`. */
	groupChanged: (subscriber: Subscriber, fn: () => void) => Dispose;
	/**
	 * Exempt a subscriber from live-delivery policy for a one-shot FETCH scan: it names one
	 * old group explicitly, so it is neither late against the live edge nor bound by the start
	 * a live subscription resolves to.
	 */
	exemptFetch: (subscriber: Subscriber) => void;
	/**
	 * Replace a serving cursor. An omitted start keeps the current floor; a provided start
	 * can lower it. Wire publishers apply SUBSCRIBE_UPDATE here rather than through
	 * `setGroups`, which never rewinds.
	 */
	replaceGroups: (subscriber: Subscriber, groups: Groups) => void;
	/**
	 * Bind a group to its track's timedness, so a frame whose timestamp disagrees is refused.
	 * Throws `TimestampMismatch` if a buffered frame already disagrees.
	 */
	bindGroupTimed: (group: GroupProducer, timed: boolean) => void;
	/** Return a group's first timestamp, retained even after its first frame is read. */
	groupTimestamp: (group: GroupConsumer) => Timestamp | undefined;
	groupLatest: (group: GroupConsumer) => Timestamp | undefined;
	/** Keep applying a subscription's drift policy after it hands a group out. */
	expireGroup: (
		group: GroupConsumer,
		expiry: { expired: () => boolean; changed: readonly Getter<unknown>[] },
	) => void;
	/** Start a group operation unless the handed-out group has expired, and stop it if the group expires mid-flight. */
	guardGroup: <T>(group: GroupConsumer, operation: () => Promise<T>) => Promise<T>;
	/** Read a frame the wire publisher completes (or skips) once written. */
	readGroupFrame: (group: GroupConsumer, from?: number) => Promise<ReadGroupFrame | undefined>;
	/** Make an evicted mirror terminal while its track timeline still contains it. */
	evictGroup: (group: GroupConsumer) => void;
	/** Attach the origin advertisement of a created broadcast. */
	attachAnnouncer: (
		producer: BroadcastProducer,
		announcer: { announce(route: Route): void; unannounce(): void; route(): Route | undefined },
	) => void;
	/** Name a broadcast handle by the path an origin created or resolved it at. */
	stampPath: (target: BroadcastProducer | BroadcastConsumer, path: Path.Valid) => void;
	/** Name the epoch of the route an origin resolved a broadcast handle through. */
	stampEpoch: (target: BroadcastConsumer, epoch: Epoch.Valid | undefined) => void;
} = {
	makeRequest: () => {
		throw new Error("track.ts not loaded");
	},
	pendingTrackProducer: () => {
		throw new Error("track.ts not loaded");
	},
	tryRecvGroup: () => {
		throw new Error("track.ts not loaded");
	},
	groupChanged: () => {
		throw new Error("track.ts not loaded");
	},
	exemptFetch: () => {
		throw new Error("track.ts not loaded");
	},
	replaceGroups: () => {
		throw new Error("track.ts not loaded");
	},
	bindGroupTimed: () => {
		throw new Error("group.ts not loaded");
	},
	groupTimestamp: () => {
		throw new Error("group.ts not loaded");
	},
	groupLatest: () => {
		throw new Error("group.ts not loaded");
	},
	expireGroup: () => {
		throw new Error("group.ts not loaded");
	},
	guardGroup: () => {
		throw new Error("group.ts not loaded");
	},
	readGroupFrame: () => {
		throw new Error("group.ts not loaded");
	},
	evictGroup: () => {
		throw new Error("group.ts not loaded");
	},
	attachAnnouncer: () => {
		throw new Error("broadcast.ts not loaded");
	},
	stampPath: () => {
		throw new Error("broadcast.ts not loaded");
	},
	stampEpoch: () => {
		throw new Error("broadcast.ts not loaded");
	},
};

/**
 * Spreads equal routes across paths: FNV-1a 64 of `path` then each hop, oldest first, as 8
 * little-endian bytes. Keyed on the requested path so an equal-cost pool advertising one
 * prefix shares its paths, and every node holding the same routes picks the same member.
 * Mirrors `fnv_key` in `rs/moq-net`; the seed is the draft's Spread Hash offset basis.
 */
export function spreadHash(path: string, hops: readonly bigint[]): bigint {
	const prime = 0x100000001b3n;
	let hash = 0x420c0decb00bn;
	for (const byte of new TextEncoder().encode(path)) {
		hash = BigInt.asUintN(64, (hash ^ BigInt(byte)) * prime);
	}
	for (const hop of hops) {
		for (let shift = 0n; shift < 64n; shift += 8n) {
			hash = BigInt.asUintN(64, (hash ^ ((hop >> shift) & 0xffn)) * prime);
		}
	}
	return hash;
}

/**
 * Package-internal constructor hooks. Classes keep their constructors private so consumers
 * can't mint detached handles; sibling modules create instances through these hooks instead.
 * Not exported from the package entrypoint.
 *
 * @module
 */
import type { Instant, Milli } from "@moq/clock";
import type { Dispose, Getter } from "@moq/signals";
import type { Consumer as BroadcastConsumer, Producer as BroadcastProducer } from "./broadcast.ts";
import type * as Epoch from "./epoch.ts";
import type { Frame, Consumer as GroupConsumer, Producer as GroupProducer } from "./group.ts";
import { isAnonymous, type Route } from "./hop.ts";
import * as Path from "./path.ts";
import type { Timestamp } from "./time.ts";
import type { Groups, Location, Producer, Request, Subscriber } from "./track.ts";
import type { Advertised, Advertisements } from "./wire.ts";

// private: plumbing
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

// pending: /quest/m1/public-model/answer.md
/**
 * The announce-interest prefix a scope needs on a prefix-shaped wire: its literal head.
 * The peer echoes every suffix beneath it, and the caller filters what arrives.
 */
export function scopeHead(scope: Path.Pattern): Path.Valid {
	return Path.from(scope.head);
}

// pending: /quest/m1/public-model/answer.md
/**
 * Whether a segment of `path` below `prefix` starts with `.`, which hides it from announce
 * discovery unless the request opts in. A path at or above the prefix never hides.
 */
export function hiddenBelow(prefix: Path.Valid, path: Path.Valid): boolean {
	const below = Path.stripPrefix(prefix, path);
	return below !== null && Path.parts(below).some((part) => part.startsWith("."));
}

// pending: /quest/m1/public-model/answer.md
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

// pending: /quest/m1/public-model/answer.md
/** Whether the announced prefix's subtree overlaps `scope`. */
export function scopeOverlaps(scope: Path.Pattern, prefix: Path.Valid): boolean {
	return scope.overlaps(Path.Pattern.subtree(prefix));
}

// pending: /quest/m1/public-model/answer.md
/** What `scope` captures from an exact announced prefix, if it pins every wildcard. */
export function scopeCaptures(scope: Path.Pattern, prefix: Path.Valid): Path.Pattern[] | undefined {
	return scope.captures(Path.Pattern.literal(prefix));
}

// private: plumbing
/** A half-open run of group sequences, `[start, end)`. */
export interface Run {
	start: number;
	end: number;
}

// private: plumbing
/** Add `[start, end)` to sorted, disjoint, non-adjacent `runs`, merging every run it touches. */
export function insertRun(runs: Run[], start: number, end: number): void {
	if (start >= end) return;
	let first = 0;
	while (first < runs.length && runs[first].end < start) first++;
	let last = first;
	while (last < runs.length && runs[last].start <= end) last++;
	const merged =
		first === last
			? { start, end }
			: { start: Math.min(start, runs[first].start), end: Math.max(end, runs[last - 1].end) };
	runs.splice(first, last - first, merged);
}

/** The sorted, disjoint `runs` with `[start, end)` taken out. */
export function removeRun(runs: readonly Run[], start: number, end: number): Run[] {
	return runs.flatMap((run) =>
		[
			{ start: run.start, end: Math.min(run.end, start) },
			{ start: Math.max(run.start, end), end: run.end },
		].filter((part) => part.start < part.end),
	);
}

// private: plumbing
/** The parts of `[start, end)` that sorted, disjoint `runs` do not cover, in order. */
export function runGaps(runs: readonly Run[], start: number, end: number): Run[] {
	const gaps: Run[] = [];
	let next = start;
	for (const run of runs) {
		if (next >= end) break;
		if (run.start > next) gaps.push({ start: next, end: Math.min(run.start, end) });
		next = Math.max(next, run.end);
	}
	if (next < end) gaps.push({ start: next, end });
	return gaps;
}

/**
 * What a non-blocking group read found, which is everything the caller needs to decide what
 * to do next: no second look at the track's closed state, and no ordering rule to get wrong.
 */
export type Recv =
	/** A group to serve. It has already left the buffer, so dropping this is dropping the group. */
	| { kind: "group"; group: GroupConsumer }
	/** Sequences `[start, end)` this cursor gave up without serving. */
	| { kind: "drop"; start: number; end: number }
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

/** One past the highest sequence written under a broadcast track's name, by any of its producers. */
export interface TrackSequence {
	next: number;
	/** Open producers writing through this namespace. */
	holders: number;
	/**
	 * The highest-sequence checked group start any producer of the name wrote, which bounds a
	 * replacement's groups the way the eviction watermark bounds a track's own.
	 */
	bound?: { sequence: number; start: Timestamp };
}

/**
 * Each name's sequence namespace for one broadcast generation, outliving the producers that
 * serve it so a replacement that would append a sequence already written is refused. Written
 * namespaces are kept until the broadcast ends, since forgetting one would let a later producer
 * reuse its sequences. Namespaces nothing holds or wrote are swept as the map grows, like the
 * Rust map at capacity, so requests for names never served stay bounded.
 */
export class TrackSequences {
	#map = new Map<string, TrackSequence>();
	// The size that triggers the next sweep, doubling past what survives so sweeps stay amortized.
	#sweepAt = 16;

	/** The namespace for `name`, created if the name is new. */
	get(name: string): TrackSequence {
		const existing = this.#map.get(name);
		if (existing) return existing;

		if (this.#map.size >= this.#sweepAt) {
			for (const [key, sequence] of this.#map) {
				if (sequence.next === 0 && sequence.holders === 0) this.#map.delete(key);
			}
			this.#sweepAt = Math.max(16, this.#map.size * 2);
		}

		const sequence = { next: 0, holders: 0 };
		this.#map.set(name, sequence);
		return sequence;
	}

	/** How many names hold a namespace. */
	get size(): number {
		return this.#map.size;
	}
}

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

// pending: /quest/m1/public-model/serve.md
/** The first group an exclusive track end excludes whole: one past the last group that may exist. */
export function finalSequence(end: Location): number {
	return end.group + (end.frame === 0 ? 0 : 1);
}

// private: plumbing
/** Hooks assigned in static blocks by the owning class. */
export const hooks: {
	// pending: /quest/m1/public-model/deferred.md
	/**
	 * Declare the end a wire subscription received: repeating it is a no-op, and a mid-group
	 * end may refine the group boundary it falls just before. Throws `ProtocolViolation` on
	 * any other second end. Readers then hold at a hole below it until the hole fills, folds
	 * past the grace, or the track closes, since QUIC may deliver a lower group's stream after
	 * a higher one. Mirrors the Rust `finish_at_pending`; assigned by `track.ts`.
	 */
	endTrack: (producer: Producer, end: Location) => void;
	// pending: /quest/m1/public-model/deferred.md
	/**
	 * Declare the first group a wire subscription's live feed serves (lite SUBSCRIBE_START, or
	 * IETF SUBSCRIBE_OK's Largest) for the floor its SUBSCRIBE requested: while the demand keeps
	 * that floor, a pending tail owes nothing below it. Mirrors the Rust `start_at`; assigned by
	 * `track.ts`.
	 */
	startTrack: (producer: Producer, served: { start: number; requested?: number }) => void;
	// pending: /quest/m1/public-model/ingest.md
	/**
	 * Mark a track as a session's copy of a remote one: its groups are never checked for a
	 * rewind, since a received timestamp may not be media time.
	 */
	receiveTrack: (producer: Producer) => void;
	/** Mint a track {@link Request}; assigned by `track.ts`. */
	makeRequest: (options: TrackRequestOptions) => Request;
	// pending: /quest/m1/public-model/ingest.md
	/** Access the existing producer while a request awaits immutable wire metadata. */
	pendingTrackProducer: (request: Request) => Producer;
	/** Have a producer write through a broadcast's sequence namespace for its name. */
	bindSequence: (producer: Producer, sequence: TrackSequence) => void;
	/** The newest frame a producer's cache holds, read without subscribing; assigned by `track.ts`. */
	cachedLargest: (producer: Producer) => Location | undefined;
	// private: capability gap
	/** Seed a received track from the publisher's reported edge; assigned by `track.ts`. */
	reportedLargest: (producer: Producer, largest: Location | undefined) => void;
	// pending: /quest/m1/public-model/serve.md
	/** Snapshot the latest group sequence for wire positioning; assigned by `track.ts`. */
	latestSequence: (subscriber: Subscriber) => number | undefined;
	// pending: /quest/m1/public-model/serve.md
	/** The newest frame buffered for a subscriber, which wire answers name; assigned by `track.ts`. */
	bufferedLargest: (subscriber: Subscriber) => Location | undefined;
	/** Call `fn` with a group's frame count now and on every change; assigned by `group.ts`. */
	groupProgress: (group: GroupProducer, fn: (total: number) => void) => Dispose;
	// pending: /quest/m1/public-model/ingest.md
	/** Record sequences `[start, end)` an upstream publisher explicitly gave up. */
	dropTrackGroups: (producer: Producer, start: number, end: number) => void;
	// pending: /quest/m1/public-model/serve.md
	/**
	 * Take the next group the subscriber's cursor allows, without waiting; assigned by `track.ts`.
	 *
	 * Synchronous so a caller can pop a group and act on it in the same turn. Park on
	 * {@link groupChanged} when it reports `idle` or `boundary`. With `dispositions`, what
	 * the cursor gives up is reported once as a `drop`.
	 */
	tryRecvGroup: (subscriber: Subscriber, dispositions: boolean) => Recv;
	// pending: /quest/m1/public-model/serve.md
	/** Wake once a subscriber's group cursor may read differently; assigned by `track.ts`. */
	groupChanged: (subscriber: Subscriber, fn: () => void) => Dispose;
	/** Apply a FETCH reader budget without inheriting SUBSCRIBE's zero default. */
	fetchBudget: (subscriber: Subscriber, maxDelay: Milli | undefined) => void;
	// pending: /quest/m1/public-model/serve.md
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
	/**
	 * Cap a group at `frames` for a track end inside it, so later writes past it throw.
	 * Throws `ProtocolViolation` if a frame at or past the cap already exists.
	 */
	limitGroup: (group: GroupProducer, frames: number) => void;
	/**
	 * Check a group's first frame against a track it joined before the frame lands. `admit`
	 * throws `TimestampMismatch` on a rewind, or returns the commit to run once every track
	 * the group joined accepted it.
	 */
	admitGroupStart: (group: GroupProducer, admit: (start: Timestamp) => () => void) => void;
	/** Return a group producer's first timestamp, if it already holds a frame. */
	groupStart: (group: GroupProducer) => Timestamp | undefined;
	/** Keep a mirror of a group accepted unverified below its track's eviction watermark out of media-time expiry. */
	unverifyGroup: (group: GroupConsumer) => void;
	/** Return a group's first timestamp, retained even after its first frame is read. */
	groupTimestamp: (group: GroupConsumer) => Timestamp | undefined;
	groupLatest: (group: GroupConsumer) => Timestamp | undefined;
	/** Return when a group joined its track at this hop, on the installed clock. */
	groupArrived: (group: GroupConsumer) => Instant;
	/** Record when a track's mirror of a group joined that track at this hop. */
	arriveGroup: (group: GroupConsumer, at: Instant) => void;
	/** Keep applying a subscription's drift policy after it hands a group out. */
	expireGroup: (
		group: GroupConsumer,
		expiry: { expired: () => boolean; changed: readonly Getter<unknown>[] },
	) => void;
	// pending: /quest/m1/public-model/serve.md
	/** Start a group operation unless the handed-out group has expired, and stop it if the group expires mid-flight. */
	guardGroup: <T>(group: GroupConsumer, operation: () => Promise<T>) => Promise<T>;
	// pending: /quest/m1/public-model/serve.md
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
	receiveTrack: () => {
		throw new Error("track.ts not loaded");
	},
	endTrack: () => {
		throw new Error("track.ts not loaded");
	},
	startTrack: () => {
		throw new Error("track.ts not loaded");
	},
	makeRequest: () => {
		throw new Error("track.ts not loaded");
	},
	pendingTrackProducer: () => {
		throw new Error("track.ts not loaded");
	},
	bindSequence: () => {
		throw new Error("track.ts not loaded");
	},
	cachedLargest: () => {
		throw new Error("track.ts not loaded");
	},
	reportedLargest: () => {
		throw new Error("track.ts not loaded");
	},
	latestSequence: () => {
		throw new Error("track.ts not loaded");
	},
	bufferedLargest: () => {
		throw new Error("track.ts not loaded");
	},
	groupProgress: () => {
		throw new Error("group.ts not loaded");
	},
	dropTrackGroups: () => {
		throw new Error("track.ts not loaded");
	},
	tryRecvGroup: () => {
		throw new Error("track.ts not loaded");
	},
	groupChanged: () => {
		throw new Error("track.ts not loaded");
	},
	fetchBudget: () => {
		throw new Error("track.ts not loaded");
	},
	replaceGroups: () => {
		throw new Error("track.ts not loaded");
	},
	bindGroupTimed: () => {
		throw new Error("group.ts not loaded");
	},
	limitGroup: () => {
		throw new Error("group.ts not loaded");
	},
	admitGroupStart: () => {
		throw new Error("group.ts not loaded");
	},
	groupStart: () => {
		throw new Error("group.ts not loaded");
	},
	unverifyGroup: () => {
		throw new Error("group.ts not loaded");
	},
	arriveGroup: () => {
		throw new Error("group.ts not loaded");
	},
	groupTimestamp: () => {
		throw new Error("group.ts not loaded");
	},
	groupLatest: () => {
		throw new Error("group.ts not loaded");
	},
	groupArrived: () => {
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

// pending: /quest/m1/public-model/answer.md
/** Covering prefixes, most specific first, including the empty root. */
export function* coveringPrefixes(path: Path.Valid): Generator<Path.Valid> {
	for (;;) {
		yield path;
		if (path === "") return;
		const slash = path.lastIndexOf("/");
		path = (slash < 0 ? "" : path.slice(0, slash)) as Path.Valid;
	}
}

/** Prefer the newer epoch, with none last. */
export function compareEpochs(a: Route, b: Route): number {
	if (a.epoch === b.epoch) return 0;
	if (a.epoch === undefined) return 1;
	if (b.epoch === undefined) return -1;
	return a.epoch > b.epoch ? -1 : 1;
}

/** Prefer newer epochs, identified publishers, then lower static cost. */
export function compareRoutes(a: Route, b: Route): number {
	return (
		compareEpochs(a, b) ||
		Number(isAnonymous(a)) - Number(isAnonymous(b)) ||
		(a.cost < b.cost ? -1 : a.cost > b.cost ? 1 : 0)
	);
}

/** Order same-prefix routes by preference, hop count, and the origin's stable spread hash. */
export function compareRouteCandidates(path: Path.Valid, a: Route, b: Route): number {
	const order = compareRoutes(a, b) || a.hops.length - b.hops.length;
	if (order !== 0) return order;
	const ha = spreadHash(path, a.hops);
	const hb = spreadHash(path, b.hops);
	return ha < hb ? -1 : ha > hb ? 1 : 0;
}

/**
 * Package-internal constructor hooks. Classes keep their constructors private so consumers
 * can't mint detached handles; sibling modules create instances through these hooks instead.
 * Not exported from the package entrypoint.
 *
 * @module
 */
import type { Instant, Milli } from "@moq/clock";
import type { Dispose, GetPromise, Getter } from "@moq/signals";
import type { Consumer as BroadcastConsumer, Producer as BroadcastProducer } from "./broadcast.ts";
import type { Datagram } from "./datagram.ts";
import type { Delivered } from "./delivered.ts";
import type * as Epoch from "./epoch.ts";
import { toStreamCode } from "./error.ts";
import type { Frame, Consumer as GroupConsumer, Producer as GroupProducer } from "./group.ts";
import { isAnonymous, type Route } from "./hop.ts";
import type { Request as OriginRequest } from "./origin.ts";
import * as Path from "./path.ts";
import type { Timestamp } from "./time.ts";
import type { Demand, Groups, Info, Position, Producer, Request, Subscriber, Subscription } from "./track.ts";
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

/** A half-open run of group sequences, `[start, end)`. */
export interface Run {
	start: number;
	end: number;
}

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

/** The sorted, disjoint `runs` with `[start, end)` taken out, each part keeping its run's fields. */
export function removeRun<R extends Run>(runs: readonly R[], start: number, end: number): R[] {
	return runs.flatMap((run) =>
		[
			{ ...run, start: run.start, end: Math.min(run.end, start) },
			{ ...run, start: Math.max(run.start, end), end: run.end },
		].filter((part) => part.start < part.end),
	);
}

/** A run of sequences given up, with the error they were given up with. */
export interface DropRun extends Run {
	error: Error;
}

/**
 * Sorted, disjoint `drops` with `[start, end)` given up with `error`, which replaces whatever
 * it overlaps. Neighbours given up with the same stream code merge, so a wide DROP stays one
 * run and a peer naming one sequence per DROP cannot grow the runs.
 */
export function insertDrop(drops: readonly DropRun[], start: number, end: number, error: Error): DropRun[] {
	if (start >= end) return [...drops];
	const next = removeRun(drops, start, end);
	let at = 0;
	while (at < next.length && next[at].start < start) at++;
	next.splice(at, 0, { start, end, error });
	const merged: DropRun[] = [];
	for (const run of next) {
		const last = merged[merged.length - 1];
		if (last && last.end === run.start && toStreamCode(last.error) === toStreamCode(run.error)) last.end = run.end;
		else merged.push({ ...run });
	}
	return merged;
}

/** The error `drops` gave `sequence` up with, if any. */
export function dropAt(drops: readonly DropRun[], sequence: number): Error | undefined {
	return drops.find((run) => run.start <= sequence && sequence < run.end)?.error;
}

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
	/** Sequences `[start, end)` this cursor gave up without serving, and why. */
	| { kind: "drop"; start: number; end: number; error: Error }
	/** Nothing readable, but the track is live and may produce more. */
	| { kind: "idle" }
	/** The producer finished, yet groups above the cap are still held: raising it releases them. */
	| { kind: "boundary" }
	/** The producer finished and the buffer is drained. Nothing can follow. */
	| { kind: "done" }
	/** The track aborted. */
	| { kind: "error"; error: Error };

/** A package-internal frame read as the group stores it. */
export interface ReadGroupFrame {
	/** Frame sequence within the group. */
	sequence: number;
	/** The frame, holding its DEFLATE slice on a compressed read. */
	frame: Frame;
	/** The stored slice's declared plain size, on compressed reads. */
	inflated?: number;
}

/** What a resumed reader's {@link ResumeSource} sees of the subscriber it serves. */
export interface ResumeShell {
	/** The reader's own preferences, mirrored into every copy it subscribes. */
	readonly subscription: Getter<Subscription | undefined>;
	/** The reader's group cursor: an inclusive start and an exclusive end. */
	readonly cursor: Getter<{ start: number; end?: number }>;
	/** Settles once the logical track ends. */
	readonly closed: GetPromise<Error | null>;
	/** The properties the first copy committed, if any yet. */
	info(): Info | undefined;
	/** Commit the logical track's properties, once. */
	commit(info: Info): void;
	/** End the logical track: cleanly, or with `abort`. */
	close(abort?: Error): void;
}

/** A drift policy a handed-out group keeps applying; see `hooks.expireGroup`. */
export interface Expiry {
	expired: () => boolean;
	changed: readonly Getter<unknown>[];
}

/**
 * Carries a handed-out group across its logical track's route changes. Consulted only once
 * the copy it reads fails, or stalls while a newer route serves, so a passthrough read never
 * touches it. The counterpart of the Rust `resume::Recover`.
 */
export interface GroupRecover {
	/** Release the route held by this group when its consumer closes. */
	close?(): void;
	/**
	 * Whether to look past the copy being read: it failed with `failed` and that is no verdict
	 * on the group, or (with none) it stalled while a newer route serves.
	 */
	wants(failed: Error | undefined): boolean;
	/** Wakes a stalled read once the route changes. */
	readonly changed: Getter<unknown>;
	/**
	 * The serving route's copy of the group, to read from frame `index` on. Rejects with the
	 * error that ends the group once nothing can continue it; abandoned once `signal` aborts.
	 */
	replace(index: number, failed: Error | undefined, signal: AbortSignal): Promise<Replacement>;
}

/** A candidate copy of a group, committed only once the read can use it. */
export interface Replacement {
	readonly group: GroupConsumer;
	/** The drift policy of the copy it came from. */
	readonly expiry: Expiry;
	/** The group reads this copy from now on. */
	adopt(): void;
	/** Nobody reads this copy: close it and give back what it holds. */
	discard(): void;
}

/** A datagram read that does not wait. */
export type RecvDatagram =
	| { kind: "datagram"; datagram: Datagram }
	| { kind: "idle" }
	| { kind: "done" }
	| { kind: "error"; error: Error };

/**
 * Which groups of a track's live feed to ask whether one may still land: one the feed has not
 * accounted for, the publisher did not drop, and the feed reaches, in a gap a later arrival
 * opened less than `grace` ago or that nothing later opened, while a producer is left.
 */
export interface Land {
	/** The first group asked about. */
	start: number;
	/** The first group past those asked about. */
	end: number;
	/** Whether the feed was asked to start at `start`: otherwise only from its first arrival. */
	floor: boolean;
	/** How long a gap a later arrival opened is waited for. */
	grace: Milli;
	/** When given, only a group it lacks counts. */
	delivered?: Delivered;
}

/**
 * The reads a resumed {@link Subscriber} delegates: it reads its routes' copies rather than a
 * buffer of its own. Implemented by `resume.ts`, the counterpart of the Rust `Inner::Resume`.
 */
export interface ResumeSource {
	/** Re-evaluate a live-edge join at the serving copy. */
	join(): void;
	/**
	 * The next group not in `delivered`, in arrival or sequence order, without waiting. With
	 * `dispositions`, a run no copy may still deliver is reported once as a `drop`.
	 */
	tryGroup(ordered: boolean, delivered: Delivered, dispositions: boolean): Recv;
	/** The next run no copy may still deliver, reported once as a `drop`, without taking a group. */
	tryDrop(delivered: Delivered): (Recv & { kind: "drop" }) | undefined;
	/**
	 * The next datagram in arrival order, without waiting. `delivered` is the group record, so a
	 * copy that still holds a group for a later group read is kept.
	 */
	tryDatagram(delivered: Delivered): RecvDatagram;
	/** Wake once a read may go differently; fires at most once. */
	changed(fn: () => void): Dispose;
	/** Forget the gaps in `delivered` below each group past the budget, judged on the newest copy. */
	forgetPastBudget(delivered: Delivered): void;
	/** The newest sequence any copy has seen. */
	latest(): number | undefined;
	/** The serving copy's declared start, raised to its cursor floor, or `"pending"` until resolved. */
	start(): number | "pending" | undefined;
	/** The logical track's declared exclusive end, as the first copy to know it declared it. */
	end(): Position | undefined;
	/** Resolve with the logical track's exclusive end once known. */
	finished(): Promise<Position>;
	/** The serving copy's newest buffered frame, which a wire answer names as its edge. */
	bufferedLargest(): Position | undefined;
	/** The serving copy's newest actual object, including metadata retained past cache eviction. */
	objectLargest(): Position | undefined;
	/** The serving copy's largest location now, and whether it can still advance. */
	peekLargest(): { largest?: Position; ended: boolean };
	/**
	 * The serving copy's largest location once within the reader's cursor and newer than `seen`,
	 * or undefined once the logical track ends.
	 */
	largest(seen: Position | undefined): Promise<Position | undefined>;
	/** An independent subscriber to the serving copy. */
	fork(options?: Subscription): Subscriber;
	/** A fresh reader for a group still cached by one of this reader's copies. */
	cachedGroup(sequence: number, from?: number): GroupConsumer | undefined;
	/** Let go of every copy. */
	close(abort?: Error): void;
}

/** Inputs for creating a package-internal track request. */
export interface TrackRequestOptions {
	/** The requested track name. */
	name: string;
	/** The producer that will serve the request. */
	producer: Producer;
	/** Requests not yet accepted or rejected by the publisher. */
	pending: Set<Request>;
}

/**
 * A requested track written before any subscriber sees it; see `hooks.stageRequest`.
 * Write through {@link producer}; {@link commit} publishes it with everything written so far.
 */
export interface StagedTrack {
	/** The track's writer. */
	readonly producer: Producer;
	/** Resolve every waiting subscriber now. Throws, rejecting the request, if the info is refused. */
	commit(): Producer;
}

/** Maximum buffered datagrams per subscriber; mirrors Rust's bounded send buffer. */
export const MAX_DATAGRAMS = 64;

/** The first group an exclusive track end excludes whole: one past the last group that may exist. */
export function finalSequence(end: Position): number {
	return end.group + (end.frame === 0 ? 0 : 1);
}

/** Hooks assigned in static blocks by the owning class. */
export const hooks: {
	/**
	 * Declare the end a wire subscription received: repeating it is a no-op, and a mid-group
	 * end may refine the group boundary it falls just before. Throws `ProtocolViolation` on
	 * any other second end. Readers then hold at a hole below it until the hole fills, folds
	 * past the grace, or the track closes, since QUIC may deliver a lower group's stream after
	 * a higher one. Mirrors the Rust `finish_at` on a `received()` copy; assigned by `track.ts`.
	 */
	endTrack: (producer: Producer, end: Position) => void;
	/**
	 * End a wire subscription's copy cleanly without declaring an end, for a session closed
	 * locally: a dead route, not finished content, so a resumed reader's replacement route
	 * continues past it. Rust aborts its copy with `Cancel` instead, which its resume also reads
	 * as a dead route. Assigned by `track.ts`.
	 */
	releaseTrack: (producer: Producer) => void;
	/**
	 * Settles once the producer is released (closed or aborted), unlike its public `closed`,
	 * which also settles on a reached end while the producer keeps serving. Assigned by `track.ts`.
	 */
	trackReleased: (producer: Producer) => GetPromise<Error | null>;
	/** {@link trackReleased} for a demand handle. Assigned by `track.ts`. */
	demandReleased: (demand: Demand) => GetPromise<Error | null>;
	/** Whether the producer declared the track's end, released or not. Assigned by `track.ts`. */
	trackFinished: (producer: Producer) => boolean;
	/**
	 * Record that the publisher gave up the groups `[start, end)` with `error`: none will
	 * arrive, so a reader waiting on one moves past it, a group still arriving in them ends with
	 * `error`, the declared end does not wait for them, and a relay serving this track forwards
	 * the drop. Mirrors the Rust `drop_groups`; assigned by `track.ts`.
	 */
	dropTrackGroups: (producer: Producer, start: number, end: number, error: Error) => void;
	/** Create a received group at its wire offset before publishing it to readers. Assigned by `track.ts`. */
	receiveGroup: (producer: Producer, group: { sequence: number; start: number }) => GroupProducer;
	/** The producer's largest reported location and its subsequent changes. */
	producerLargest: (producer: Producer) => Getter<Position | undefined>;

	/** Re-evaluate a live-edge join at this subscriber's copy. */
	/**
	 * The track's exclusive end as this subscriber's copy knows it now, for a publisher
	 * forwarding it. Readers await `finished()` instead. Assigned by `track.ts`.
	 */
	subscriberEnd: (subscriber: Subscriber) => Position | undefined;
	joinSubscriber: (subscriber: Subscriber) => void;
	/** Re-evaluate a live-edge join at a producer. */
	joinProducer: (producer: Producer) => void;

	/**
	 * Declare the first group a wire subscription's live feed serves (lite SUBSCRIBE_START, or
	 * IETF SUBSCRIBE_OK's Largest) for the floor its SUBSCRIBE requested: while the demand keeps
	 * that floor, a pending tail owes nothing below it. Mirrors the Rust `start_at`; assigned by
	 * `track.ts`. A null `start` resolves an empty range; without `start`, the SUBSCRIBE still
	 * waits on its START, as the Rust `request_start`.
	 */
	startTrack: (producer: Producer, served: { start?: number | null; requested?: number }) => void;
	/** Mint a track {@link Request}; assigned by `track.ts`. */
	makeRequest: (options: TrackRequestOptions) => Request;
	/**
	 * Take a request's producer without resolving its subscribers, so content written before
	 * {@link StagedTrack.commit} is there when the first reader looks; assigned by `track.ts`.
	 */
	stageRequest: (request: Request, info: Partial<Info>) => StagedTrack;
	/** The newest frame a producer's cache holds, read without subscribing; assigned by `track.ts`. */
	cachedLargest: (producer: Producer) => Position | undefined;
	/** The publisher priority from a demanded track's committed {@link Info}; assigned by `track.ts`. */
	demandPriority: (demand: Demand) => number;
	/** Seed a received track from the publisher's reported edge; assigned by `track.ts`. */
	reportedLargest: (producer: Producer, largest: Position | undefined) => void;
	/**
	 * Seed a received track from the publisher's reported group alone, whose frame is unknown,
	 * so it names no object; assigned by `track.ts`.
	 */
	reportedGroup: (producer: Producer, group: number) => void;
	/** Snapshot the latest group sequence for wire positioning; assigned by `track.ts`. */
	latestSequence: (subscriber: Subscriber) => number | undefined;
	/**
	 * The source's declared start, raised to the subscriber's cursor floor, or `"pending"` while
	 * the upstream START that resolves it has yet to arrive; assigned by `track.ts`.
	 */
	sourceStart: (subscriber: Subscriber) => number | "pending" | undefined;
	/** The newest frame still buffered for a subscriber; assigned by `track.ts`. */
	bufferedLargest: (subscriber: Subscriber) => Position | undefined;
	/** The newest actual object received or reported upstream, independent of cache retention. */
	objectLargest: (subscriber: Subscriber) => Position | undefined;
	/** Call `fn` with a group's frame count now and on every change; assigned by `group.ts`. */
	groupProgress: (group: GroupProducer, fn: (total: number) => void) => Dispose;
	/**
	 * Take the next group the subscriber's cursor allows, without waiting; assigned by `track.ts`.
	 *
	 * Synchronous so a caller can pop a group and act on it in the same turn. Park on
	 * {@link groupChanged} when it reports `idle` or `boundary`. With `dispositions`, what
	 * the cursor gives up is reported once as a `drop`.
	 */
	tryRecvGroup: (subscriber: Subscriber, dispositions: boolean) => Recv;
	/**
	 * The next run the subscriber's cursor gave up and has not reported, without taking a group.
	 * From the first call on, the budget skips of {@link tryRecvGroup} without dispositions are
	 * kept for it, so a resumed reader takes groups the plain way and drops from here.
	 */
	tryDrop: (subscriber: Subscriber) => (Recv & { kind: "drop" }) | undefined;
	/** The sequence {@link tryNextGroup} (`ordered`) or {@link tryRecvGroup} would take next, without taking it. */
	peekGroup: (subscriber: Subscriber, ordered: boolean) => number | undefined;
	/** Take the next group in sequence order without waiting, as {@link tryRecvGroup} does in arrival order. */
	tryNextGroup: (subscriber: Subscriber) => Recv;
	/** Wake once a subscriber's datagram cursor may read differently; fires at most once. */
	datagramChanged: (subscriber: Subscriber, fn: () => void) => Dispose;
	/** Take the next datagram in arrival order without waiting. */
	tryRecvDatagram: (subscriber: Subscriber) => RecvDatagram;
	/** Whether a buffered group in `[start, end)` that `delivered` lacks still waits to be read. */
	hasUnreadGroup: (subscriber: Subscriber, start: number, end: number | undefined, delivered: Delivered) => boolean;
	/**
	 * Whether a subscriber's track ended and it holds nothing left to read: no datagram within its
	 * cursor, and no group in `[start, end)` that `delivered` lacks.
	 */
	spent: (subscriber: Subscriber, start: number, end: number | undefined, delivered: Delivered) => boolean;
	/** How many runs of handed-out groups a subscriber remembers: one per gap, plus the floor. Tests only. */
	deliveredRuns: (subscriber: Subscriber) => number;
	/** Whether the subscriber already handed out group `sequence`, or no longer expects it. */
	handedOut: (subscriber: Subscriber, sequence: number) => boolean;
	/** Forget the gaps in another reader's `delivered` below each group past this subscriber's budget. */
	forgetPastBudget: (subscriber: Subscriber, delivered: Delivered) => void;
	/**
	 * Take `[start, end)` back out of what a subscriber's wire cursor reported dropped: a drop
	 * below where SUBSCRIBE_START resolved never goes out, so a wider budget may still serve it.
	 */
	unreport: (subscriber: Subscriber, start: number, end: number) => void;
	/** Whether this subscriber's budget gives up on `sequence`, judged by its successor; see {@link expireGroup}. */
	staleAt: (subscriber: Subscriber, sequence: number) => Expiry;
	/** A group the subscriber's track still caches, as a fresh read handle. */
	cachedGroup: (subscriber: Subscriber, sequence: number, from?: number) => GroupConsumer | undefined;
	/** Whether {@link cachedGroup} would find the group, without making a read handle. */
	holdsGroup: (subscriber: Subscriber, sequence: number) => boolean;
	/** The error the subscriber's track gave `sequence` up with, if it did, and what changes it. */
	givenUp: (subscriber: Subscriber, sequence: number) => { error?: Error; changed: Getter<unknown> };
	/** Whether a group may still land on the live feed of the subscriber's track; see {@link Land}. */
	mayLand: (subscriber: Subscriber, land: Land) => boolean;
	/** A subscriber's largest location now, and whether it can still advance. */
	peekLargest: (subscriber: Subscriber) => { largest?: Position; ended: boolean };
	/** Wake once a subscriber's largest location or its end may have changed; fires at most once. */
	largestChanged: (subscriber: Subscriber, fn: () => void) => Dispose;
	/** A subscriber that reads its routes' copies through `source` instead of a buffer of its own. */
	resumeSubscriber: (
		name: string,
		subscription: Subscription | undefined,
		source: (shell: ResumeShell) => ResumeSource,
	) => Subscriber;
	/** Wake once a subscriber's group cursor may read differently, including when the track declares its end; assigned by `track.ts`. */
	groupChanged: (subscriber: Subscriber, fn: () => void) => Dispose;
	/** Apply a FETCH reader budget without inheriting SUBSCRIBE's zero default. */
	fetchBudget: (subscriber: Subscriber, maxDelay: Milli | undefined) => void;
	/**
	 * Bind a group to its track's timedness, so a frame whose timestamp disagrees is refused.
	 * Throws `TimestampMismatch` if a buffered frame already disagrees.
	 */
	bindGroupTimed: (group: GroupProducer, timed: boolean) => void;
	/** Admit a datagram in its stored wire form. */
	writeDatagram: (producer: Producer, datagram: Datagram, inflated?: number) => void;
	/** Bind the group to its immutable track codec. */
	bindGroupCompression: (group: GroupProducer, compressed: boolean) => void;
	/** Admit a received stored slice without recompressing it. */
	/** Set a received group's first absolute frame before publishing it. Assigned by `group.ts`. */
	startGroup: (group: GroupProducer, start: number) => void;
	/** The first absolute frame this source copy began at. Assigned by `group.ts`. */
	groupOffset: (group: GroupProducer) => number;
	writeGroupFrame: (group: GroupProducer, frame: Frame, inflated?: number) => void;
	/** Payload footprint including a shared inflated copy, for measurements. */
	groupCacheBytes: (group: GroupProducer) => number;
	/** Frame count without creating a read handle. */
	groupFrameCount: (group: GroupProducer) => number;
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
	/** Whether a group's bytes after its first frame exceed `ratio` times the first frame. */
	overRefresh: (group: GroupProducer, ratio: number) => boolean;
	/** Keep a mirror of a group accepted unverified below its track's eviction watermark out of media-time expiry. */
	unverifyGroup: (group: GroupConsumer) => void;
	/** Return a group's first timestamp, retained even after its first frame is read. */
	/**
	 * Release a group reader, failing it with `abort` if given, by closing its own mirrored copy.
	 * Assigned by `group.ts`.
	 */
	releaseGroupReader: (group: GroupConsumer, abort?: Error) => void;
	groupTimestamp: (group: GroupConsumer) => Timestamp | undefined;
	groupLatest: (group: GroupConsumer) => Timestamp | undefined;
	/** Return when a group joined its track at this hop, on the installed clock. */
	groupArrived: (group: GroupConsumer) => Instant;
	/** Whether a group counts as content: its first frame landed, or it ended. */
	/** The next absolute frame this reader can serve. Assigned by `group.ts`. */
	groupIndex: (group: GroupConsumer) => number;
	groupStarted: (group: GroupConsumer) => boolean;
	/** Watch the error a group's copy closes with, ignoring readers' verdicts, without holding the consumer. */
	groupFate: (group: GroupConsumer) => () => Error | undefined;
	/** Record when a track's mirror of a group joined that track at this hop. */
	arriveGroup: (group: GroupConsumer, at: Instant) => void;
	/** Keep applying a subscription's drift policy after it hands a group out. */
	expireGroup: (group: GroupConsumer, expiry: Expiry) => void;
	/** Carry a handed-out group across its logical track's route changes. */
	recoverGroup: (group: GroupConsumer, recover: GroupRecover) => void;
	/** Read the next frame, as its stored DEFLATE slice when `compressed`, so a lite-07 publisher forwards it without inflating. */
	readGroupFrame: (group: GroupConsumer, compressed: boolean) => Promise<ReadGroupFrame | undefined>;
	/** Make an evicted mirror terminal while its track timeline still contains it. */
	evictGroup: (group: GroupConsumer) => void;
	/** Attach the origin advertisement of a created broadcast. */
	attachAnnouncer: (
		producer: BroadcastProducer,
		announcer: { announce(route: Route): void; unannounce(): void; route(): Route | undefined },
	) => void;

	/** Call `fn` synchronously when an open broadcast closes, from any handle. */
	onClosed: (producer: BroadcastProducer, fn: () => void) => Dispose;
	/** Name a broadcast handle by the path an origin created or resolved it at. */
	stampPath: (target: BroadcastProducer | BroadcastConsumer, path: Path.Valid) => void;
	/** Name the epoch of the route an origin resolved a broadcast handle through. */
	stampEpoch: (target: BroadcastConsumer, epoch: Epoch.Valid | undefined) => void;
	/** Mark a broadcast handle served through a route claiming a shorter prefix: it lists nothing. */
	stampUnlisted: (target: BroadcastConsumer) => void;
	/**
	 * Resolve a session-forwarded request with its peer's routed broadcast. An application
	 * claim answers through `prepare` and `publish` instead. Assigned by `origin.ts`.
	 */
	acceptRequest: (request: OriginRequest, source: BroadcastProducer | BroadcastConsumer) => void;
} = {
	endTrack: () => {
		throw new Error("track.ts not loaded");
	},
	releaseTrack: () => {
		throw new Error("track.ts not loaded");
	},
	trackReleased: () => {
		throw new Error("track.ts not loaded");
	},
	demandReleased: () => {
		throw new Error("track.ts not loaded");
	},
	trackFinished: () => {
		throw new Error("track.ts not loaded");
	},
	dropTrackGroups: () => {
		throw new Error("track.ts not loaded");
	},
	producerLargest: () => {
		throw new Error("track.ts not loaded");
	},
	joinProducer: () => {
		throw new Error("track.ts not loaded");
	},
	subscriberEnd: () => {
		throw new Error("track.ts not loaded");
	},
	joinSubscriber: () => {
		throw new Error("track.ts not loaded");
	},
	startTrack: () => {
		throw new Error("track.ts not loaded");
	},
	makeRequest: () => {
		throw new Error("track.ts not loaded");
	},
	stageRequest: () => {
		throw new Error("track.ts not loaded");
	},
	cachedLargest: () => {
		throw new Error("track.ts not loaded");
	},
	demandPriority: () => {
		throw new Error("track.ts not loaded");
	},
	reportedLargest: () => {
		throw new Error("track.ts not loaded");
	},
	reportedGroup: () => {
		throw new Error("track.ts not loaded");
	},
	latestSequence: () => {
		throw new Error("track.ts not loaded");
	},
	sourceStart: () => {
		throw new Error("track.ts not loaded");
	},
	bufferedLargest: () => {
		throw new Error("track.ts not loaded");
	},
	objectLargest: () => {
		throw new Error("track.ts not loaded");
	},
	groupProgress: () => {
		throw new Error("group.ts not loaded");
	},
	tryRecvGroup: () => {
		throw new Error("track.ts not loaded");
	},
	tryDrop: () => {
		throw new Error("track.ts not loaded");
	},
	tryNextGroup: () => {
		throw new Error("track.ts not loaded");
	},
	peekGroup: () => {
		throw new Error("track.ts not loaded");
	},
	forgetPastBudget: () => {
		throw new Error("track.ts not loaded");
	},
	unreport: () => {
		throw new Error("track.ts not loaded");
	},
	tryRecvDatagram: () => {
		throw new Error("track.ts not loaded");
	},
	deliveredRuns: () => {
		throw new Error("track.ts not loaded");
	},
	handedOut: () => {
		throw new Error("track.ts not loaded");
	},
	datagramChanged: () => {
		throw new Error("track.ts not loaded");
	},
	hasUnreadGroup: () => {
		throw new Error("track.ts not loaded");
	},
	spent: () => {
		throw new Error("track.ts not loaded");
	},
	staleAt: () => {
		throw new Error("track.ts not loaded");
	},
	cachedGroup: () => {
		throw new Error("track.ts not loaded");
	},
	holdsGroup: () => {
		throw new Error("track.ts not loaded");
	},
	givenUp: () => {
		throw new Error("track.ts not loaded");
	},
	mayLand: () => {
		throw new Error("track.ts not loaded");
	},
	peekLargest: () => {
		throw new Error("track.ts not loaded");
	},
	largestChanged: () => {
		throw new Error("track.ts not loaded");
	},
	resumeSubscriber: () => {
		throw new Error("track.ts not loaded");
	},
	groupChanged: () => {
		throw new Error("track.ts not loaded");
	},
	fetchBudget: () => {
		throw new Error("track.ts not loaded");
	},
	writeDatagram: () => {
		throw new Error("track.ts not loaded");
	},
	bindGroupCompression: () => {
		throw new Error("group.ts not loaded");
	},
	receiveGroup: () => {
		throw new Error("track.ts not loaded");
	},
	groupOffset: () => {
		throw new Error("group.ts not loaded");
	},
	startGroup: () => {
		throw new Error("group.ts not loaded");
	},
	writeGroupFrame: () => {
		throw new Error("group.ts not loaded");
	},
	groupCacheBytes: () => {
		throw new Error("group.ts not loaded");
	},
	groupFrameCount: () => {
		throw new Error("group.ts not loaded");
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
	overRefresh: () => {
		throw new Error("group.ts not loaded");
	},
	unverifyGroup: () => {
		throw new Error("group.ts not loaded");
	},
	arriveGroup: () => {
		throw new Error("group.ts not loaded");
	},
	releaseGroupReader: () => {
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
	groupIndex: () => {
		throw new Error("group.ts not loaded");
	},
	groupStarted: () => {
		throw new Error("group.ts not loaded");
	},
	groupFate: () => {
		throw new Error("group.ts not loaded");
	},
	expireGroup: () => {
		throw new Error("group.ts not loaded");
	},
	recoverGroup: () => {
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

	onClosed: () => {
		throw new Error("broadcast.ts not loaded");
	},
	stampPath: () => {
		throw new Error("broadcast.ts not loaded");
	},
	stampEpoch: () => {
		throw new Error("broadcast.ts not loaded");
	},
	stampUnlisted: () => {
		throw new Error("broadcast.ts not loaded");
	},
	acceptRequest: () => {
		throw new Error("origin.ts not loaded");
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

/**
 * Order routes at `prefix` resolving `path` by preference, hop count, and the stable spread
 * hash. The hash only spreads a prefix's paths: at the prefix itself a tie is left for the
 * newest to win, so a publisher that reconnects under a new hop takes over from its lingering
 * session at once, matching `route_order` in rs/moq-net for routes a node did not get from a
 * cluster peer, which is every route here.
 */
export function compareRouteCandidates(path: Path.Valid, prefix: Path.Valid, a: Route, b: Route): number {
	const order = compareRoutes(a, b) || a.hops.length - b.hops.length;
	if (order !== 0 || path === prefix) return order;
	const ha = spreadHash(path, a.hops);
	const hb = spreadHash(path, b.hops);
	return ha < hb ? -1 : ha > hb ? 1 : 0;
}

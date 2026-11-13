/**
 * Package-internal constructor hooks. Classes keep their constructors private so consumers
 * can't mint detached handles; sibling modules create instances through these hooks instead.
 * Not exported from the package entrypoint.
 *
 * @module
 */
import type { Duration, Instant } from "@moq/clock";
import type { Dispose, GetPromise, Getter } from "@moq/signals";
import type { Consumer as BroadcastConsumer, Producer as BroadcastProducer } from "./broadcast.ts";
import type { Reader as CacheReader, Charge, Pool } from "./cache.ts";
import type * as datagram from "./datagram.ts";
import type { Delivered } from "./delivered.ts";
import { StreamCode, StreamError, toStreamCode } from "./error.ts";
import type { Consumer as FrameConsumer } from "./frame.ts";
import type { Frame, Consumer as GroupConsumer, Producer as GroupProducer } from "./group.ts";
import { isAnonymous, type WireRoute } from "./hop.ts";
import * as Instance from "./instance.ts";
import type { Consumer as OriginConsumer, Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";
import type { Timestamp } from "./time.ts";
import type {
	Delay,
	Demand,
	Groups,
	Info,
	InfoInit,
	Position,
	Producer,
	Request,
	Subscriber,
	Subscription,
	WireRecord,
} from "./track.ts";
import { max, min } from "./util/sequence.ts";
import * as Varint from "./varint.ts";
import type { Resolved, Window } from "./window.ts";
import type { Advertised, Advertisements } from "./wire.ts";

/** What a null {@link Delay.max} goes out as, in milliseconds: the largest value every peer can read, as Rust sends for `None`. */
export const NO_MAX_DELAY = Varint.MAX_U53;

/** A datagram track's properties as the stream track info a wire answers with: no groups, so no age, refresh, or compression. */
export function datagramTrackInfo(info: datagram.Info): Info {
	return { compression: "none", timescale: info.timescale, maxAge: null, priority: info.priority, refresh: null };
}

/** A subscription's {@link Delay.max} in milliseconds as the wire carries it: null for no limit, omitted for zero. */
export function maxDelayOf(subscription: Pick<Subscription, "delay">): number {
	const max = subscription.delay?.max;
	if (max === null) return NO_MAX_DELAY;
	return max?.asMillis() ?? 0;
}

/** A subscription's {@link Delay.min} in milliseconds as the wire carries it: undefined when not stated, omitted for zero. */
export function minDelayOf(subscription: Pick<Subscription, "delay">): number | undefined {
	const min = subscription.delay?.min;
	if (min === null) return undefined;
	return min?.asMillis() ?? 0;
}

/** Normalize public group bounds into an inclusive start and exclusive end. */
export function groupBounds(groups: Groups = {}): { start: bigint; end?: bigint } {
	const bound = (value: Groups["start"] | undefined, start: boolean): bigint | undefined => {
		if (value === undefined) return undefined;
		if ((value.included === undefined) === (value.excluded === undefined)) {
			throw new Error("a group bound must be either included or excluded");
		}
		const sequence = value.included ?? value.excluded;
		if (typeof sequence !== "bigint" || sequence < 0n) {
			throw new Error("a group bound must be a non-negative bigint");
		}
		return sequence + ((start ? value.excluded !== undefined : value.included !== undefined) ? 1n : 0n);
	};
	return { start: bound(groups.start, true) ?? 0n, end: bound(groups.end, false) };
}

/**
 * The announce-interest prefix a scope needs on a prefix-shaped wire: its literal head.
 * The peer echoes every suffix beneath it, and the caller filters what arrives.
 *
 * A head cut at an escaped `/`, such as `a\` from `a\/*`, ends in half an escape, so it
 * falls back to its parent.
 */
export function scopeHead(scope: Path.Pattern): Path.Valid {
	let head = scope.head;
	for (;;) {
		try {
			return Path.from(head);
		} catch {
			head = head.slice(0, Math.max(head.lastIndexOf("/"), 0));
		}
	}
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
			const snap = firstEligible(
				candidates,
				(candidate) => !candidate.claim || candidate.claim.overlaps(requested),
			);
			if (!snap) continue;
			rootLen = covered.length;
			out.set(Path.empty(), snap);
			continue;
		}
		const suffix = Path.stripPrefix(prefix, covered);
		const best = suffix !== null ? firstEligible(candidates) : undefined;
		if (suffix !== null && best) out.set(suffix, best);
	}
	return out;
}

/**
 * The first of `candidates`, most preferred first, that `admits` and that no other admitted
 * candidate supersedes. Supersession is decided among what the reader may see, so a higher epoch
 * it cannot see never hides a route it can.
 */
export function firstEligible<T extends { readonly route: WireRoute; readonly prefix?: Path.Valid }>(
	candidates: readonly T[],
	admits: (candidate: T) => boolean = () => true,
): T | undefined {
	const admitted = candidates.filter(admits);
	const byPrefix = new Map<Path.Valid | undefined, WireRoute[]>();
	for (const candidate of admitted) {
		const routes = byPrefix.get(candidate.prefix);
		if (routes) routes.push(candidate.route);
		else byPrefix.set(candidate.prefix, [candidate.route]);
	}
	const epochs = new Map([...byPrefix].map(([prefix, routes]) => [prefix, new Epochs(routes)]));
	return admitted.find((candidate) => !epochs.get(candidate.prefix)?.supersedes(candidate.route));
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
	start: bigint;
	end: bigint;
}

/** Add `[start, end)` to sorted, disjoint, non-adjacent `runs`, merging every run it touches. */
export function insertRun(runs: Run[], start: bigint, end: bigint): void {
	if (start >= end) return;
	// Merging a run it touches, so the first that ends at or past `start`.
	const first = runsFrom(runs, start - 1n);
	let last = first;
	while (last < runs.length && runs[last].start <= end) last++;
	const merged =
		first === last ? { start, end } : { start: min(start, runs[first].start), end: max(end, runs[last - 1].end) };
	runs.splice(first, last - first, merged);
}

/** The sorted, disjoint `runs` with `[start, end)` taken out, each part keeping its run's fields. */
export function removeRun<R extends Run>(runs: readonly R[], start: bigint, end: bigint): R[] {
	return runs.flatMap((run) =>
		[
			{ ...run, start: run.start, end: min(run.end, start) },
			{ ...run, start: max(run.start, end), end: run.end },
		].filter((part) => part.start < part.end),
	);
}

/** Take `[start, end)` out of sorted, disjoint `runs` in place, each part keeping its run's fields. */
export function cutRun<R extends Run>(runs: R[], start: bigint, end: bigint): void {
	if (start >= end) return;
	const from = runsFrom(runs, start);
	let to = from;
	while (to < runs.length && runs[to].start < end) to++;
	if (from === to) return;
	const parts: R[] = [];
	if (runs[from].start < start) parts.push({ ...runs[from], end: start });
	if (runs[to - 1].end > end) parts.push({ ...runs[to - 1], start: end });
	runs.splice(from, to - from, ...parts);
}

/**
 * Why a group sequence will not be delivered. Mirrors the Rust `track::Hole`.
 *
 * lite-07 carries the two kinds in their own messages, SUBSCRIBE_SKIP and SUBSCRIBE_DROP;
 * earlier versions name both in SUBSCRIBE_DROP.
 */
export interface Hole {
	/** The error a reader waiting on the sequence ends with: NOT_FOUND for a skip. */
	error: Error;
	/** Never produced, such as a skipped sequence number: final, and it ends no reader. */
	skip?: boolean;
	/**
	 * The budget in milliseconds a staleness verdict was judged under, undefined for a drop that
	 * does not depend on it. Only a widening past it reverses the verdict.
	 */
	stale?: number;
}

/** A sequence never produced; see {@link Hole.skip}. */
export function skipHole(): Hole {
	return { error: new StreamError(StreamCode.NotFound, { message: "group never produced" }), skip: true };
}

/** A group judged stale under `budget` milliseconds. */
export function staleHole(budget: number): Hole {
	return { error: new StreamError(StreamCode.Old, { message: "group is past the budget" }), stale: budget };
}

/** A run of sequences given up, with why. */
export interface DropRun extends Run, Hole {
	/** Copied into a cursor from the track's runs, which it folds in afresh on each change. */
	received?: boolean;
}

/** Whether two runs say the same thing, so they may merge. */
export function sameHole(a: DropRun, b: DropRun): boolean {
	return (
		toStreamCode(a.error) === toStreamCode(b.error) &&
		!a.skip === !b.skip &&
		a.stale === b.stale &&
		!a.received === !b.received
	);
}

/**
 * Sorted, disjoint `drops` with `[start, end)` given up as `hole` says, which replaces whatever
 * it overlaps. Neighbours given up alike merge, so a wide DROP stays one run and a peer naming
 * one sequence per DROP cannot grow the runs.
 */
export function insertDrop(
	drops: readonly DropRun[],
	start: bigint,
	end: bigint,
	hole: Hole & { received?: boolean },
): DropRun[] {
	if (start >= end) return [...drops];
	const next = removeRun(drops, start, end);
	let at = 0;
	while (at < next.length && next[at].start < start) at++;
	next.splice(at, 0, { start, end, error: hole.error, skip: hole.skip, stale: hole.stale, received: hole.received });
	const merged: DropRun[] = [];
	for (const run of next) {
		const last = merged[merged.length - 1];
		if (last && last.end === run.start && sameHole(last, run)) last.end = run.end;
		else merged.push({ ...run });
	}
	return merged;
}

/**
 * The parts of sorted, disjoint `runs` that sorted, disjoint `cut` leaves, each keeping its run's
 * fields, as {@link removeRun} of each cut would, in one pass.
 */
export function subtractRuns<R extends Run>(runs: readonly R[], cut: readonly Run[]): R[] {
	const kept: R[] = [];
	let first = 0;
	for (const run of runs) {
		while (first < cut.length && cut[first].end <= run.start) first++;
		let from = run.start;
		for (let i = first; i < cut.length && cut[i].start < run.end; i++) {
			if (cut[i].start > from) kept.push({ ...run, start: from, end: cut[i].start });
			from = max(from, cut[i].end);
		}
		if (from < run.end) kept.push({ ...run, start: from, end: run.end });
	}
	return kept;
}

/**
 * `drops` with each of the sorted, disjoint `runs` inserted as {@link insertDrop} inserts it, in
 * one pass: folding in a span of many runs costs the runs, not their square.
 */
export function insertDrops(drops: readonly DropRun[], runs: readonly DropRun[]): DropRun[] {
	if (runs.length === 0) return [...drops];
	const next = subtractRuns(drops, runs);
	// Appended one by one: a peer controls how many runs there are, past any argument limit.
	for (const run of runs) next.push(run);
	next.sort((a, b) => (a.start < b.start ? -1 : a.start > b.start ? 1 : 0));
	const merged: DropRun[] = [];
	for (const run of next) {
		const last = merged[merged.length - 1];
		if (last && last.end === run.start && sameHole(last, run)) last.end = run.end;
		else merged.push({ ...run });
	}
	return merged;
}

/** The error `drops` gave `sequence` up with, if any. */
export function dropAt(drops: readonly DropRun[], sequence: bigint): Error | undefined {
	return drops.find((run) => run.start <= sequence && sequence < run.end)?.error;
}

/** The index of the first of sorted, disjoint `runs` ending past `sequence`. */
export function runsFrom(runs: readonly Run[], sequence: bigint): number {
	let lo = 0;
	let hi = runs.length;
	while (lo < hi) {
		const mid = (lo + hi) >>> 1;
		if (runs[mid].end <= sequence) lo = mid + 1;
		else hi = mid;
	}
	return lo;
}

/** The parts of `[start, end)` that sorted, disjoint `runs` do not cover, in order. */
export function runGaps(runs: readonly Run[], start: bigint, end: bigint): Run[] {
	const gaps: Run[] = [];
	let next = start;
	for (let i = runsFrom(runs, start); i < runs.length; i++) {
		const run = runs[i];
		if (next >= end) break;
		if (run.start > next) gaps.push({ start: next, end: min(run.start, end) });
		next = max(next, run.end);
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
	| ({ kind: "drop"; start: bigint; end: bigint } & Hole)
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
	/** The payload to stream instead of `frame.payload`, for a frame written in pieces. */
	chunks?: FrameConsumer;
}

/** What a resumed reader's {@link ResumeSource} sees of the subscriber it serves. */
export interface ResumeShell {
	/** The reader's own preferences, mirrored into every copy it subscribes. */
	readonly subscription: Getter<Subscription | undefined>;
	/** The reader's group cursor: an inclusive start and an exclusive end. */
	readonly cursor: Getter<{ start: bigint; end?: bigint }>;
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

/** A copy of a group still arriving, held for a duplicate that waits on it. */
export interface Arriving {
	/** How the copy ends. */
	readonly settled: GetPromise<Error | null>;
	/** Stop holding it, once the duplicate took the group or gave up. Idempotent. */
	release(): void;
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

/**
 * Which groups of a track's live feed to ask whether one may still land: one the feed has not
 * accounted for, the publisher did not drop, and the feed reaches, in a gap a later arrival
 * opened less than `grace` ago or that nothing later opened, while a producer is left.
 */
export interface Land {
	/** The first group asked about. */
	start: bigint;
	/** The first group past those asked about. */
	end: bigint;
	/** Whether the feed was asked to start at `start`: otherwise only from its first arrival. */
	floor: boolean;
	/** How long a gap a later arrival opened is waited for. */
	grace: Duration;
	/** When given, only a group it lacks counts. */
	delivered?: Delivered;
}

/**
 * The reads a resumed {@link Subscriber} delegates: it reads its routes' copies rather than a
 * buffer of its own. Implemented by `resume.ts`, the counterpart of the Rust `Inner::Resume`.
 */
export interface ResumeSource {
	/** Observe a serving copy's abort, including delivery failures a resumable reader waits past. */
	watchError(fn: (error: Error) => void): Dispose;
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
	 * Whether the reader's bounded `window` has nothing left to deliver, judged by the serving
	 * copy against what it and the logical reader (`delivered`) handed out. A copy that may still
	 * deliver keeps it open. Mirrors the Rust `Reader::exhausted`.
	 */
	exhausted(delivered: Delivered, window: Window): boolean;
	/** Wake once a read may go differently; fires at most once. */
	changed(fn: () => void): Dispose;
	/** Forget the gaps in `delivered` below each group past the budget, judged on the newest copy. */
	forgetPastBudget(delivered: Delivered): void;
	/** The newest sequence any copy has seen. */
	latest(): bigint | undefined;
	/** The serving copy's declared start, raised to its cursor floor, or `"pending"` until resolved. */
	start(): bigint | "pending" | undefined;
	/** The logical track's declared exclusive end, as the first copy to know it declared it. */
	end(): Position | undefined;
	/** Where each range starts on the serving copy once delivery starts at `from`, undefined without one. */
	starts(from: bigint): (bigint | undefined)[] | undefined;
	/** Each range as the serving copy resolved it, undefined without one. */
	resolved(): (Resolved | undefined)[] | undefined;
	/** The runs of `[start, end)` the serving copy reads, undefined without one. */
	runs(start: bigint, end: bigint): Run[] | undefined;
	/**
	 * The exclusive end past every group the serving copy reads: undefined while one of its
	 * ranges stays open or without a serving copy.
	 */
	servingEnd(): bigint | undefined;
	/** The error the logical track's end carries, if it does not end cleanly. */
	endError(): Error | undefined;
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
	cachedGroup(sequence: bigint, from?: number): GroupConsumer | undefined;
	/** See {@link hooks.reofferGroups}, answered by the serving copy. */
	reofferGroups(start: bigint, end: bigint, from?: number): { groups: GroupConsumer[]; givenUp: Run[] };
	/**
	 * See {@link hooks.reofferStale}: the stale runs queued here, those this reader reported, which a
	 * route change keeps, and those the serving copy reported.
	 */
	reofferStale(): Run[];
	/**
	 * The bill for the reader's record on the serving copy's account: `current` while it bills that
	 * account, or else a new one. `current` without a serving copy.
	 */
	bill(current: CacheReader | undefined): CacheReader | undefined;
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
	/** Metadata-only readers holding this request, if its broadcast supports them. */
	lookups?: { pending: Getter<number>; opened: boolean };
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

/**
 * How a received copy's start bounds the fills its readers ask below it. Mirrors the Rust
 * `Start::Resolved` and `Producer::fills_from_start`.
 *
 * Below a start the publisher resolved from the budget (`resolvesStart`), a reader fills the
 * history its own budget reaches: an update can cross the response, so nobody knows which
 * budget picked that start. On a wire whose fills and timestamps cannot judge an older group
 * against that budget (`fillsFromStart`, lite-06), readers fill nothing below the start, and a
 * widened budget reaches lower groups through the feed's re-offer. Anywhere else, a reader fills
 * below the start only the groups its ranges name.
 */
export interface FillStarts {
	/** The wire's start is resolved from the subscriber's budget: lite SUBSCRIBE_START from lite-06. */
	resolvesStart?: boolean;
	/** Readers ask no fills below the start at all. */
	fillsFromStart?: boolean;
}

/** The first group an exclusive track end excludes whole: one past the last group that may exist. */
export function finalSequence(end: Position): bigint {
	return end.group + (end.frame === 0 ? 0n : 1n);
}

/** Hooks assigned in static blocks by the owning class. */
export const hooks: {
	/** Make a request for an unanswered datagram track. */
	datagramRequest: (producer: datagram.Producer, pending: Set<datagram.Request>) => datagram.Request;
	/** End a received datagram track if its last consumer left. */
	closeUnusedDatagrams: (producer: datagram.Producer) => boolean;
	/** Follow this consumer's subscriber priority for transport scheduling. */
	datagramPriority: (consumer: datagram.Consumer) => Getter<number>;
	/** Forward data carried by an existing stream without contributing another subscription's demand. */
	observeDatagrams: (producer: datagram.Producer) => datagram.Consumer;
	/** Settles once this consumer is closed, the end of whatever request it serves. */
	datagramClosed: (consumer: datagram.Consumer) => GetPromise<Error | null>;
	/** Whether two consumers read the same datagram track. */
	datagramSameTrack: (a: datagram.Consumer, b: datagram.Consumer) => boolean;
	/** Another consumer at the same position that adds no demand, until its priority is set. */
	observeDatagramConsumer: (consumer: datagram.Consumer) => datagram.Consumer;
	/**
	 * Declare the end a wire subscription received: repeating it is a no-op, and a mid-group
	 * end may refine the group boundary it falls just before. Throws `ProtocolViolation` on
	 * any other second end. Readers then hold at a hole below it until the hole fills, folds
	 * past the grace, or the track closes, since QUIC may deliver a lower group's stream after
	 * a higher one. Mirrors the Rust `finish_at` on a `received()` copy; assigned by `track.ts`.
	 */
	endTrack: (producer: Producer, end: Position, error?: Error) => void;
	/**
	 * End a wire subscription's copy cleanly without declaring an end, for a session closed
	 * locally: a dead route, not finished content, so a resumed reader's replacement route
	 * continues past it. Rust aborts its copy with `Cancel` instead, which its resume also reads
	 * as a dead route. Assigned by `track.ts`.
	 */
	releaseTrack: (producer: Producer) => void;
	/** Cache a new track's groups into its broadcast's `pool`, before it caches any. Assigned by `track.ts`. */
	poolTrack: (producer: Producer, pool: Pool) => void;
	/** How many runs a track's arrivals hold, for tests. Assigned by `track.ts`. */
	arrivalRuns: (producer: Producer) => number;
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
	 * the drop. A skip ends no reader, and replaces any drop; a drop leaves a skip in place.
	 * Returns the runs recorded. Mirrors the Rust `give_up`; assigned by `track.ts`.
	 */
	dropTrackGroups: (producer: Producer, start: bigint, end: bigint, hole: Hole) => Run[];
	/**
	 * `Producer.skipGroups`, returning the cached sequences it left alone, which a skip naming a
	 * group that arrived contradicts. Mirrors the Rust `skip_received`; assigned by `track.ts`.
	 */
	skipTrackGroups: (producer: Producer, start: bigint, end: bigint) => Run[];
	/**
	 * Forget every run a received copy holds as stale: the terms its upstream judged them under
	 * were replaced, so they are unknown again. `judged` is every group the new terms' ranges name,
	 * undefined when unknown. Returns the runs forgotten: a group that arrived since its stale drop
	 * replaced it, so it is not among them. Mirrors the Rust `supersede_stale`.
	 */
	supersedeStale: (producer: Producer, judged: readonly Run[] | undefined) => Run[];
	/**
	 * Mark a track as fed by a wire subscription, which owes missing groups, until
	 * {@link cacheTrack} ends it. `starts` says how that wire's start bounds the fills its readers
	 * ask below it. Assigned by `track.ts`.
	 */
	receiveTrack: (producer: Producer, starts?: FillStarts) => void;
	/**
	 * Create a received group at its wire offset before publishing it to readers; `fill` marks one a
	 * fill's subscription answered, which the feed never promised. Assigned by `track.ts`.
	 */
	receiveGroup: (producer: Producer, group: { sequence: bigint; start: number; fill?: boolean }) => GroupProducer;
	/**
	 * Hold the copy of `sequence` still arriving, if one is, for a duplicate to wait on: until
	 * released, that copy's failure leaves its readers open for the duplicate to continue. Assigned
	 * by `track.ts`.
	 */
	arrivingGroup: (producer: Producer, sequence: bigint) => Arriving | undefined;
	/** The producer's largest reported location and its subsequent changes. */
	producerLargest: (producer: Producer) => Getter<Position | undefined>;

	/** Re-evaluate a live-edge join at this subscriber's copy. */
	/**
	 * The track's exclusive end as this subscriber's copy knows it now, for a publisher
	 * forwarding it. Readers await `finished()` instead. Assigned by `track.ts`.
	 */
	subscriberEnd: (subscriber: Subscriber) => Position | undefined;
	/** The error that end carries, if the track does not end cleanly. Assigned by `track.ts`. */
	subscriberEndError: (subscriber: Subscriber) => Error | undefined;
	joinSubscriber: (subscriber: Subscriber) => void;
	/** Re-evaluate a live-edge join at a producer. */
	joinProducer: (producer: Producer) => void;

	/**
	 * Declare the first group a wire subscription's live feed serves (lite SUBSCRIBE_START, or
	 * IETF SUBSCRIBE_OK's Largest) for the floor its SUBSCRIBE requested: while the demand keeps
	 * that floor, a pending tail owes nothing below it. Mirrors the Rust `start_at`; assigned by
	 * `track.ts`. A null `start` resolves an empty range; without `start`, the SUBSCRIBE still
	 * waits on its START, as the Rust `request_start`. Only after {@link receiveTrack}: the start
	 * is that subscription's, and ends with it.
	 */
	startTrack: (producer: Producer, served: { start?: bigint | null; requested?: bigint }) => void;
	/**
	 * Mark a track as a copy received over a wire without ranges, which cannot carry a relative
	 * anchor other than the live start, so its readers refuse one rather than guess from a
	 * partial copy. Mirrors the Rust `Holdings::drops_anchors`; assigned by `track.ts`.
	 */
	dropAnchors: (producer: Producer) => void;
	/** Mint a track {@link Request}; assigned by `track.ts`. */
	makeRequest: (options: TrackRequestOptions) => Request;
	/**
	 * Take a request's producer without resolving its subscribers, so content written before
	 * {@link StagedTrack.commit} is there when the first reader looks; assigned by `track.ts`.
	 */
	stageRequest: (request: Request, info: InfoInit) => StagedTrack;
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
	reportedGroup: (producer: Producer, group: bigint) => void;
	/**
	 * How long a session's tail waits for a late group: the subscribers' max delay, bounded by
	 * the track's max age; assigned by `track.ts`.
	 */
	tailGrace: (producer: Producer) => Duration;
	/** Snapshot the latest group sequence for wire positioning; assigned by `track.ts`. */
	latestSequence: (subscriber: Subscriber) => bigint | undefined;
	/**
	 * The source's declared start, raised to the subscriber's cursor floor, or `"pending"` while
	 * the upstream START that resolves it has yet to arrive; assigned by `track.ts`.
	 */
	sourceStart: (subscriber: Subscriber) => bigint | "pending" | undefined;
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
	/**
	 * The runs of `[start, end)` a subscriber reads: inside its ranges and its local bounds.
	 * Assigned by `track.ts`.
	 */
	servingRuns: (subscriber: Subscriber, start: bigint, end: bigint) => Run[];
	/**
	 * The exclusive end past every group a subscriber reads, or undefined while one of its ranges
	 * stays open. Assigned by `track.ts`.
	 */
	servingEnd: (subscriber: Subscriber) => bigint | undefined;
	/**
	 * Where each of a subscriber's ranges starts once delivery starts at `from`, undefined for one
	 * with no group there: what lite-07 SUBSCRIBE_START answers. Assigned by `track.ts`.
	 */
	servingStarts: (subscriber: Subscriber, from: bigint) => (bigint | undefined)[];
	/**
	 * Each of a subscriber's ranges as resolved where it is served, undefined for one whose start
	 * still waits: what lite-07 SUBSCRIBE_STARTED answers. Assigned by `track.ts`.
	 */
	servingResolved: (subscriber: Subscriber) => (Resolved | undefined)[];
	/** The lowest buffered candidate, which an ordered take may wait below; `floor` passes sequences taken on other copies. */
	peekGroup: (subscriber: Subscriber, ordered: boolean, floor?: bigint) => bigint | undefined;
	/** Take the next group in sequence order without waiting, as {@link tryRecvGroup} does in arrival order. */
	tryNextGroup: (subscriber: Subscriber, floor?: bigint) => Recv;
	/** Whether a buffered group in `[start, end)` that `delivered` lacks still waits to be read. */
	hasUnreadGroup: (subscriber: Subscriber, start: bigint, end: bigint | undefined, delivered: Delivered) => boolean;
	/**
	 * Whether a subscriber's track ended and it holds nothing left to read: no group in `[start, end)` that `delivered` lacks.
	 */
	spent: (subscriber: Subscriber, start: bigint, end: bigint | undefined, delivered: Delivered) => boolean;
	/** Whether a subscriber's last read waits on a fill that still owes it a group. Mirrors the Rust `awaits_fill`. */
	awaitsFill: (subscriber: Subscriber) => boolean;
	/**
	 * Mark a wire publisher's reader to park past the end of a bounded window rather than end the
	 * read there, since its peer reads the end of a stream as the track's. Mirrors the Rust
	 * crate-private `track::Subscriber::parked`. Returns the subscriber.
	 */
	parked: (subscriber: Subscriber) => Subscriber;
	/**
	 * Whether a route copy's bounded window has nothing left for the logical reader that handed
	 * out `delivered`, counting what the copy handed out itself. Mirrors the Rust `exhausted_for`.
	 */
	exhaustedFor: (subscriber: Subscriber, delivered: Delivered) => boolean;
	/** How many runs of handed-out groups a subscriber remembers: one per gap, plus the floor. Tests only. */
	deliveredRuns: (subscriber: Subscriber) => number;
	/** Whether the subscriber already handed out group `sequence`, or no longer expects it. */
	handedOut: (subscriber: Subscriber, sequence: bigint) => boolean;
	/** Forget the gaps in another reader's `delivered` below each group past this subscriber's budget. */
	forgetPastBudget: (subscriber: Subscriber, delivered: Delivered) => void;
	/**
	 * What a lite publisher told its wire about a subscriber's sequences, kept in the
	 * subscriber's own record. Mirrors the Rust crate-private `track::Subscriber::record_mut`.
	 */
	wire: (subscriber: Subscriber) => WireRecord;
	/**
	 * Open a bill against a track's cache account for what a session keeps for it beside the
	 * cache, such as its tail's record of finished streams, billed as a reader's record is, so
	 * the pool can hand it debt. Mirrors the Rust crate-private `track::Producer::bill`.
	 */
	billTrack: (producer: Producer) => CacheReader;
	/**
	 * The bill on the account of the track `subscriber` reads: `current` while it bills that account,
	 * or else a new one; undefined while it has none.
	 */
	billSubscriber: (subscriber: Subscriber, current: CacheReader | undefined) => CacheReader | undefined;
	/**
	 * Re-offer what a subscriber gave up as stale through dispositions, as a wire whose every update
	 * reopens its stale drops does: each run is judged again under the new terms. Returns the runs
	 * taken back, which are owed again. Mirrors the Rust crate-private
	 * `track::Subscriber::reoffer_stale`.
	 */
	reofferStale: (subscriber: Subscriber) => Run[];
	/**
	 * Fresh reads of the groups in `[start, end)` a subscriber's track still caches, its range still
	 * admits, and its current budget no longer judges stale, each expiring against that budget, and
	 * the runs still given up: for a wire whose update re-offers a group whose stream it gave up as
	 * stale, or a lite-07 continuation of a group whose stream yielded. Each read starts at frame
	 * `from`, so a group whose cache holds only frames past it is given up. Mirrors the Rust
	 * crate-private `track::Subscriber::reoffer_groups`.
	 */
	reofferGroups: (
		subscriber: Subscriber,
		start: bigint,
		end: bigint,
		from?: number,
	) => { groups: GroupConsumer[]; givenUp: Run[] };
	/** Whether this subscriber's budget gives up on `sequence`, judged by its successor; see {@link expireGroup}. */
	staleAt: (subscriber: Subscriber, sequence: bigint) => Expiry;
	/** A group the subscriber's track still caches, as a fresh read handle. */
	cachedGroup: (subscriber: Subscriber, sequence: bigint, from?: number) => GroupConsumer | undefined;
	/** Whether {@link cachedGroup} would find the group, without making a read handle. */
	holdsGroup: (subscriber: Subscriber, sequence: bigint) => boolean;
	/** The error the subscriber's track gave `sequence` up with, if it did, and what changes it. */
	givenUp: (subscriber: Subscriber, sequence: bigint) => { error?: Error; changed: Getter<unknown>[] };
	/**
	 * The parts of `[start, end)` the subscriber's track still holds as a received stale run: one
	 * new terms superseded is gone. Mirrors the Rust crate-private `track::Subscriber::stale_held`.
	 */
	staleHeld: (subscriber: Subscriber, start: bigint, end: bigint) => Run[];
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
	fetchBudget: (subscriber: Subscriber, maxDelay: Duration | undefined) => void;
	/** Whether a reader beyond one-shot fetches subscribes to this copy. */
	subscribing: (producer: Producer) => Getter<boolean>;
	/** The aggregate across every reader, one-shot fetches included: what the wire asks for. */
	requested: (producer: Producer) => Getter<Subscription | undefined>;
	/** Leave a wire copy serving its cache and fills without a live feed. */
	cacheTrack: (producer: Producer) => void;
	/** Fail subscriptions reaching beyond `edge`, keeping history readers and one-shot fetches. */
	refuseLive: (producer: Producer, edge: bigint, error: Error) => void;
	/**
	 * A fetch's group `sequence`, before it reads: `true` once it is cached or asked of a fill,
	 * written yet or not, as the Rust `fetch_group` queues one; `false` with no handler to ask;
	 * `"pending"` while the track awaits its accept, which opens its handler.
	 */
	askFetch: (subscriber: Subscriber, sequence: bigint) => boolean | "pending";
	/**
	 * Bind a group to its track's info, so a frame whose timestamp doesn't `rescale` to the track
	 * is refused. Throws `TimestampMismatch` if a buffered frame already disagrees.
	 */
	bindGroupTimed: (group: GroupProducer, track: Info) => void;
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
	/** Bill a cached group's bytes to `charge` from now on, or stop billing them with undefined. */
	chargeGroup: (group: GroupProducer, charge: Charge | undefined) => void;
	/**
	 * Shed whole frames from a group's front toward a cache `debt`, freeing about `cap` at most,
	 * and end the `readers` its track holds that lost a shed frame; returns the bytes freed.
	 */
	shedGroup: (group: GroupProducer, debt: number, cap: number, readers: Iterable<GroupConsumer>) => number;
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
	/** Call `fn` as a group aborts, before its readers can look; assigned by `group.ts`. */
	onGroupAbort: (group: GroupProducer, fn: (error: Error) => void) => void;
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
	/** Settles once a group reader read or released everything it holds, its {@link GroupConsumer.done}. Assigned by `group.ts`. */
	groupDone: (group: GroupConsumer) => GetPromise<Error | null>;
	/** Whether a group reader still reads the copy it was mirrored from, not one its recovery moved it to. */
	readsMirrored: (group: GroupConsumer) => boolean;
	/**
	 * Whether the reader a track buffered `group` for will still read it: only then does it keep
	 * its source group wanted. Assigned by `group.ts`.
	 */
	groupReads: (group: GroupConsumer, reads: boolean) => void;
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
	/** The failure that ends a group's reader, as its `closed` would settle, without building that handle. */
	groupFailed: (group: GroupConsumer) => Error | undefined;
	/**
	 * Carry a group across its logical track's route changes. Attached before the group is handed
	 * out, so its `closed` never settles on a failure the recovery continues past.
	 */
	recoverGroup: (group: GroupConsumer, recover: GroupRecover) => void;
	/**
	 * Hold a received copy still arriving for a duplicate: if it fails while held, its readers wait
	 * for the duplicate to take over (see {@link Arriving}). Assigned by `group.ts`.
	 */
	holdGroup: (group: GroupProducer) => () => void;
	/**
	 * Hand `next` the readers `failed` kept open while held, teeing each from the frame it reached.
	 * Fails those `next` cannot serve. Returns the bytes of `failed`'s frames they still hold.
	 * Assigned by `group.ts`.
	 */
	succeedGroup: (next: GroupProducer, failed: GroupProducer) => number;
	/** Read a payload chunk, releasing its wait when the output stream closes. */
	readFrameChunk: (frame: FrameConsumer, closed: Promise<void>) => Promise<Uint8Array | undefined>;
	/** Read the next frame, as its stored DEFLATE slice when `compressed`, so a lite-07 publisher forwards it without inflating. */
	readGroupFrame: (group: GroupConsumer, compressed: boolean, from?: number) => Promise<ReadGroupFrame | undefined>;
	/** Notify the owning track when a reader reaches its shed front. */
	onShedRead: (group: GroupConsumer, notify: () => void) => void;
	/**
	 * Make an evicted mirror terminal while its track timeline still contains it, failing its
	 * unread content with `lost`. Whether the mirror is released too: not when its group reads
	 * on, or may yet, from another copy.
	 */
	evictGroup: (group: GroupConsumer, lost: Error) => boolean;
	/** Record a cache access on a group a reader takes, restarting its idle window. */
	refreshGroup: (group: GroupConsumer) => void;
	/** Call `fn` synchronously when an open broadcast closes, from any handle. */
	onClosed: (producer: BroadcastProducer, fn: () => void) => Dispose;
	/** Reserve an on-demand track without listing it or enabling a request handler. */
	reserveUnlistedTrack: (producer: BroadcastProducer, name: string) => Request;
	/** Whether a subscriber or a metadata lookup still holds a requested track. */
	requestWanted: (request: Request) => Getter<boolean>;
	/** Transfer a metadata-opened request to its session's producer lifetime. */
	keepRequest: (request: Request) => void;
	/** Observe the peer's reset without waiting for routed delivery recovery. */
	watchSubscriberError: (subscriber: Subscriber, fn: (error: Error) => void) => Dispose;
	/**
	 * Count a session withholding the origin's hidden broadcasts from its peer, such as a
	 * moq-lite client before 07, whose peer can't ask for them, until disposed.
	 */
	withholdHidden: (origin: OriginConsumer) => Dispose;
	/** How many sessions withhold the origin's hidden broadcasts from their peer. */
	hiddenWithheld: (origin: OriginProducer) => Getter<number>;
	/** Name a broadcast handle by the path an origin created or resolved it at. */
	stampPath: (target: BroadcastProducer | BroadcastConsumer, path: Path.Valid) => void;
	/** Name the instance of the route an origin resolved a broadcast handle through. */
	stampInstance: (target: BroadcastConsumer, instance: Instance.Valid | undefined) => void;
	/** Mark a broadcast handle served through a route claiming a shorter prefix: it lists nothing. */
	stampUnlisted: (target: BroadcastConsumer) => void;
	/** Whether a broadcast handle was {@link stampUnlisted marked unlisted}. */
	unlisted: (target: BroadcastConsumer) => boolean;
} = {
	datagramRequest: () => {
		throw new Error("datagram module not loaded");
	},
	closeUnusedDatagrams: () => {
		throw new Error("datagram module not loaded");
	},
	observeDatagrams: () => {
		throw new Error("datagram module not loaded");
	},
	datagramPriority: () => {
		throw new Error("datagram module not loaded");
	},
	datagramClosed: () => {
		throw new Error("datagram module not loaded");
	},
	datagramSameTrack: () => {
		throw new Error("datagram module not loaded");
	},
	observeDatagramConsumer: () => {
		throw new Error("datagram module not loaded");
	},
	endTrack: () => {
		throw new Error("track.ts not loaded");
	},
	releaseTrack: () => {
		throw new Error("track.ts not loaded");
	},
	poolTrack: () => {
		throw new Error("track.ts not loaded");
	},
	arrivalRuns: () => {
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
	skipTrackGroups: () => {
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
	subscriberEndError: () => {
		throw new Error("track.ts not loaded");
	},
	joinSubscriber: () => {
		throw new Error("track.ts not loaded");
	},
	startTrack: () => {
		throw new Error("track.ts not loaded");
	},
	dropAnchors: () => {
		throw new Error("track.ts not loaded");
	},
	servingRuns: () => {
		throw new Error("track.ts not loaded");
	},
	servingEnd: () => {
		throw new Error("track.ts not loaded");
	},
	servingStarts: () => {
		throw new Error("track.ts not loaded");
	},
	servingResolved: () => {
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
	tailGrace: () => {
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
	wire: () => {
		throw new Error("track.ts not loaded");
	},
	billTrack: () => {
		throw new Error("track.ts not loaded");
	},
	billSubscriber: () => {
		throw new Error("track.ts not loaded");
	},
	supersedeStale: () => {
		throw new Error("track.ts not loaded");
	},
	reofferStale: () => {
		throw new Error("track.ts not loaded");
	},
	reofferGroups: () => {
		throw new Error("track.ts not loaded");
	},
	deliveredRuns: () => {
		throw new Error("track.ts not loaded");
	},
	handedOut: () => {
		throw new Error("track.ts not loaded");
	},
	hasUnreadGroup: () => {
		throw new Error("track.ts not loaded");
	},
	spent: () => {
		throw new Error("track.ts not loaded");
	},
	awaitsFill: () => {
		throw new Error("track.ts not loaded");
	},
	parked: () => {
		throw new Error("track.ts not loaded");
	},
	exhaustedFor: () => {
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
	staleHeld: () => {
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
	cacheTrack: () => {
		throw new Error("track.ts not loaded");
	},
	refuseLive: () => {
		throw new Error("track.ts not loaded");
	},
	subscribing: () => {
		throw new Error("track module not loaded");
	},
	requested: () => {
		throw new Error("track.ts not loaded");
	},
	fetchBudget: () => {
		throw new Error("track.ts not loaded");
	},
	askFetch: () => {
		throw new Error("track.ts not loaded");
	},
	bindGroupCompression: () => {
		throw new Error("group.ts not loaded");
	},
	receiveTrack: () => {
		throw new Error("track.ts not loaded");
	},
	receiveGroup: () => {
		throw new Error("track.ts not loaded");
	},
	arrivingGroup: () => {
		throw new Error("track.ts not loaded");
	},
	succeedGroup: () => {
		throw new Error("group.ts not loaded");
	},
	holdGroup: () => {
		throw new Error("group.ts not loaded");
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
	chargeGroup: () => {
		throw new Error("group.ts not loaded");
	},
	shedGroup: () => {
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
	onGroupAbort: () => {
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
	groupDone: () => {
		throw new Error("group.ts not loaded");
	},
	readsMirrored: () => {
		throw new Error("group.ts not loaded");
	},
	groupReads: () => {
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
	groupFailed: () => {
		throw new Error("group.ts not loaded");
	},
	recoverGroup: () => {
		throw new Error("group.ts not loaded");
	},
	readFrameChunk: () => {
		throw new Error("group.ts not loaded");
	},
	readGroupFrame: () => {
		throw new Error("group.ts not loaded");
	},
	onShedRead: () => {
		throw new Error("group.ts not loaded");
	},
	evictGroup: () => {
		throw new Error("group.ts not loaded");
	},
	refreshGroup: () => {
		throw new Error("group.ts not loaded");
	},
	onClosed: () => {
		throw new Error("broadcast.ts not loaded");
	},
	reserveUnlistedTrack: () => {
		throw new Error("broadcast.ts not loaded");
	},
	requestWanted: () => {
		throw new Error("track.ts not loaded");
	},
	keepRequest: () => {
		throw new Error("track.ts not loaded");
	},
	watchSubscriberError: () => {
		throw new Error("track.ts not loaded");
	},
	withholdHidden: () => {
		throw new Error("origin.ts not loaded");
	},
	hiddenWithheld: () => {
		throw new Error("origin.ts not loaded");
	},
	stampPath: () => {
		throw new Error("broadcast.ts not loaded");
	},
	stampInstance: () => {
		throw new Error("broadcast.ts not loaded");
	},
	stampUnlisted: () => {
		throw new Error("broadcast.ts not loaded");
	},
	unlisted: () => {
		throw new Error("broadcast.ts not loaded");
	},
};

/** FNV-1a 64 of `path`, then `bytes`. The seed is the draft's Spread Hash offset basis. */
function fnv(path: string, bytes: Iterable<number>): bigint {
	const prime = 0x100000001b3n;
	let hash = 0x420c0decb00bn;
	for (const byte of new TextEncoder().encode(path)) {
		hash = BigInt.asUintN(64, (hash ^ BigInt(byte)) * prime);
	}
	for (const byte of bytes) {
		hash = BigInt.asUintN(64, (hash ^ BigInt(byte)) * prime);
	}
	return hash;
}

/**
 * Spreads a pool's instances across paths: FNV-1a 64 of `path` then the instance's UTF-8.
 * Keyed on the requested path so a pool advertising one prefix shares its paths, and every
 * node agrees, since the instance reads the same everywhere. Mirrors `instance_key` in
 * `rs/moq-net`.
 */
export function spreadHash(path: string, instance: string): bigint {
	return fnv(path, new TextEncoder().encode(instance));
}

/**
 * The draft's Rendezvous Hash: FNV-1a 64 of `path` then the origin's Node ID as 8 little-endian
 * bytes, so every node that knows the same lite-07 claims ranks them alike. Mirrors the node key
 * of `route_order` in `rs/moq-net`.
 */
export function rendezvousHash(path: string, node: bigint): bigint {
	const bytes: number[] = [];
	for (let shift = 0n; shift < 64n; shift += 8n) bytes.push(Number((node >> shift) & 0xffn));
	return fnv(path, bytes);
}

/**
 * Settles among one instance's routes: FNV-1a 64 of `path` then each hop, oldest first, as 8
 * little-endian bytes. Mirrors `chain_key` in `rs/moq-net`.
 */
export function chainHash(path: string, hops: readonly bigint[]): bigint {
	const bytes: number[] = [];
	for (const hop of hops) {
		for (let shift = 0n; shift < 64n; shift += 8n) bytes.push(Number((hop >> shift) & 0xffn));
	}
	return fnv(path, bytes);
}

/**
 * The best of `candidates`, oldest first, at `prefix` for resolving `path`: `best_route` in
 * rs/moq-net. Only an instance's highest epoch is eligible, ranked by
 * {@link compareRouteCandidates} (`route_order`), and a full tie goes to the newest, as Rust's
 * final `Reverse(entry.id)`: a claim is new when it starts or restarts (another instance or
 * epoch), and keeps its place through a re-price. Every selector ranks through this, so the
 * announcements and the consumes of one session never pick apart.
 */
export function bestCandidate<T>(
	path: Path.Valid,
	prefix: Path.Valid,
	candidates: Iterable<T>,
	route: (candidate: T) => WireRoute,
): T | undefined {
	const list = [...candidates];
	const epochs = new Epochs(list.map(route));
	let best: T | undefined;
	for (const candidate of list) {
		const next = route(candidate);
		if (epochs.supersedes(next)) continue;
		if (best === undefined || compareRouteCandidates(path, prefix, next, route(best)) <= 0) best = candidate;
	}
	return best;
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

/**
 * Whether `route` at `prefix` is superseded among `others`: one of them carries its instance at
 * a higher epoch at the same prefix. An older incarnation of an instance is never eligible,
 * however cheap, since its publisher was replaced.
 */
export function superseded(route: WireRoute, others: Iterable<WireRoute>): boolean {
	return new Epochs(others).supersedes(route);
}

/**
 * The highest epoch each instance reaches among a set of routes, built in one pass so ranking
 * N routes stays linear. Only epochs above 0 are kept, since nothing is below 0: the common
 * case, where every instance announces epoch 0, stores nothing. Mirrors `Epochs` in rs/moq-net.
 */
export class Epochs {
	readonly #highest = new Map<Instance.Valid | undefined, bigint>();

	constructor(routes: Iterable<WireRoute>) {
		for (const route of routes) {
			const epoch = route.epoch ?? 0n;
			if (epoch === 0n) continue;
			const highest = this.#highest.get(route.instance);
			if (highest === undefined || epoch > highest) this.#highest.set(route.instance, epoch);
		}
	}

	/** Whether a route carries `route`'s instance at a higher epoch. */
	supersedes(route: WireRoute): boolean {
		const highest = this.#highest.get(route.instance);
		return highest !== undefined && highest > (route.epoch ?? 0n);
	}
}

/**
 * Prefer a route that is neither draining (a peer that sent GOAWAY) nor unavailable (held),
 * which rank below every other, then identified publishers, then lower static cost.
 */
export function compareRoutes(a: WireRoute, b: WireRoute): number {
	const last = (route: WireRoute) => Number((route.draining ?? false) || route.available === false);
	return (
		last(a) - last(b) ||
		Number(a.node === undefined && isAnonymous(a)) - Number(b.node === undefined && isAnonymous(b)) ||
		(a.cost < b.cost ? -1 : a.cost > b.cost ? 1 : 0)
	);
}

/**
 * Order eligible routes at `prefix` resolving `path` by preference, hop count, then the spread
 * hash of each instance, then, within one instance, the hash of its chain. A lite-07 route counts
 * as the one hop to its origin node, and ranks by the rendezvous hash of that node instead. The
 * hashes only spread a prefix's paths: at the prefix itself a tie is left for the newest to win
 * (see {@link bestCandidate}), so a publisher that restarts under a new instance takes over from
 * its lingering session at once, matching `route_order` in rs/moq-net for routes a node did not
 * get from a cluster peer, which is every route here.
 */
export function compareRouteCandidates(path: Path.Valid, prefix: Path.Valid, a: WireRoute, b: WireRoute): number {
	const order = compareRoutes(a, b) || routeHops(a) - routeHops(b);
	if (order !== 0 || path === prefix) return order;
	const [a1, a2] = routeKey(path, a);
	const [b1, b2] = routeKey(path, b);
	if (a1 !== b1) return a1 < b1 ? -1 : 1;
	return a2 < b2 ? -1 : a2 > b2 ? 1 : 0;
}

/**
 * The hops a route ranks by: a lite-07 route counts as the one hop to its origin node, since the
 * rest of its chain is the lite-06 one it carries only for loop detection. As `route_order` in
 * rs/moq-net.
 */
export function routeHops(route: WireRoute): number {
	return route.node !== undefined ? 1 : route.hops.length;
}

/**
 * The hash pair `route_order` in rs/moq-net ranks a route by beneath its prefix, each route keyed
 * on its own: a lite-07 claim by the rendezvous hash of its origin node, any other by the spread
 * hash of its instance and then the hash of its chain.
 */
function routeKey(path: Path.Valid, route: WireRoute): [bigint, bigint] {
	if (route.node !== undefined) return [rendezvousHash(path, route.node), 0n];
	return [spreadHash(path, route.instance ? Instance.text(route.instance) : ""), chainHash(path, route.hops)];
}

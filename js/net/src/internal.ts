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
import type { Charge, Pool } from "./cache.ts";
import type * as datagram from "./datagram.ts";
import type { Delivered } from "./delivered.ts";
import type { Edit, Prefs } from "./demand.ts";
import { StreamCode, StreamError, toStreamCode } from "./error.ts";
import type { Consumer as FrameConsumer } from "./frame.ts";
import type { Frame, Consumer as GroupConsumer, Producer as GroupProducer } from "./group.ts";
import { isAnonymous, type WireRoute } from "./hop.ts";
import * as Instance from "./instance.ts";
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
import { max, min } from "./util/key.ts";
import * as Varint from "./varint.ts";
import type { Holdings, Resolved } from "./window.ts";
import type { Advertised, Advertisements } from "./wire.ts";

/** A null {@link Delay.max} in milliseconds, which the model holds as the largest value every peer can read. */
export const NO_MAX_DELAY = Varint.MAX_U53;

/** A datagram track's properties as the stream track info a wire answers with: no groups, so no age, refresh, or compression. */
export function datagramTrackInfo(info: datagram.Info): Info {
	return {
		compression: "none",
		timescale: info.timescale,
		maxAge: null,
		priority: info.priority,
		refresh: { ratio: null, interval: null },
	};
}

/**
 * What asks a track for a new group, mirroring the Rust `track::Ask`: a reader joining at the
 * live edge (judged by the ratio), a {@link Subscriber.refresh} or a peer's Wanted Start past
 * the largest, or a reader of the newest group reaching a frame its copy shed.
 */
export type Ask = "join" | "refresh" | "shed";

/** A subscription's {@link Delay.max} in milliseconds as the wire carries it: null for no limit, omitted for zero. */
export function maxDelayOf(subscription: Pick<Subscription, "delay">): number {
	const max = subscription.delay?.max;
	if (max === null) return NO_MAX_DELAY;
	return max?.asMillis() ?? 0;
}

/** A subscription's {@link Delay.max} in milliseconds as SUBSCRIBE carries it: null for no limit. */
export function wireMaxDelay(subscription: Pick<Subscription, "delay">): number | null {
	const max = maxDelayOf(subscription);
	return max < NO_MAX_DELAY ? max : null;
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
		const key = value.included ?? value.excluded;
		if (typeof key !== "bigint" || key < 0n) {
			throw new Error("a group bound must be a non-negative bigint");
		}
		return key + ((start ? value.excluded !== undefined : value.included !== undefined) ? 1n : 0n);
	};
	return { start: bound(groups.start, true) ?? 0n, end: bound(groups.end, false) };
}

/**
 * The announce-interest prefix a scope needs on a prefix-shaped wire: its literal head.
 * The peer echoes every suffix beneath it, and the caller filters what arrives. A pattern's
 * literal segments are spelled like a path's, so its head is always a path.
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
 * wins the way a request through the prefix would resolve. Only a candidate `shows` admits
 * at the path it lands on competes.
 */
export function presented(
	prefix: Path.Valid,
	table: Advertisements,
	carries: (covered: Path.Valid) => boolean,
	// Lite-07: a route above the prefix keeps how many segments broader than the prefix it is.
	keepAbove = false,
	shows: (candidate: Advertised, at: Path.Valid) => boolean = () => true,
): Map<Path.Valid, Advertised> {
	const out = new Map<Path.Valid, Advertised>();
	// The most specific claim covering the prefix wins, then the deeper filing, as a request resolves.
	let rootRank: [number, number] | undefined;
	for (const [covered, candidates] of table) {
		if (!carries(covered)) continue;
		if (Path.hasPrefix(covered, prefix)) {
			const snap = firstEligible(candidates, (candidate) => shows(candidate, prefix));
			if (!snap) continue;
			const depth = Path.parts(covered).length;
			const rank: [number, number] = [depth - Number(snap.route.above ?? 0n), depth];
			if (rootRank && (rank[0] < rootRank[0] || (rank[0] === rootRank[0] && rank[1] < rootRank[1]))) continue;
			rootRank = rank;
			const above = BigInt(Path.parts(prefix).length - depth);
			out.set(
				Path.empty(),
				keepAbove && above > 0n
					? { ...snap, route: { ...snap.route, above: (snap.route.above ?? 0n) + above } }
					: snap,
			);
			continue;
		}
		const suffix = Path.stripPrefix(prefix, covered);
		const best = suffix !== null ? firstEligible(candidates, (candidate) => shows(candidate, covered)) : undefined;
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

/**
 * What of `path`'s subtree every one of `scopes` serves: undefined when all of it, and otherwise
 * the prefixes beneath `path` that all of them share, possibly none. Asked about a path rather
 * than a {@link Path.Prefix}, so a path no prefix can spell still gets an answer.
 */
export function within(path: Path.Valid, scopes: Iterable<Path.Prefixes | undefined>): Path.Prefixes | undefined {
	let out: Path.Prefixes | undefined;
	for (const scope of scopes) {
		if (!scope || scope.matches(path)) continue;
		const beneath = new Path.Prefixes(
			scope.toArray().filter((member) => Path.hasPrefix(path, member.text as Path.Valid)),
		);
		out = out ? out.intersect(beneath) : beneath;
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

/** A half-open run of group keys, `[start, end)`. */
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
 * Why a group key will not be delivered. Mirrors the Rust `track::Hole`.
 *
 * lite-07 carries the two kinds in their own messages, SUBSCRIBE_SKIP and SUBSCRIBE_DROP;
 * earlier versions name both in SUBSCRIBE_DROP.
 */
export interface Hole {
	/** The error a reader waiting on the key ends with: NOT_FOUND for a skip. */
	error: Error;
	/** Never produced, such as a skipped sequence number: final, and it ends no reader. */
	skip?: boolean;
	/**
	 * The budget in milliseconds a staleness verdict was judged under, undefined for a drop that
	 * does not depend on it. Only a widening past it reverses the verdict.
	 */
	stale?: number;
}

/** A key never produced; see {@link Hole.skip}. */
export function skipHole(): Hole {
	return { error: new StreamError(StreamCode.NotFound, { message: "group never produced" }), skip: true };
}

/** A group judged stale under `budget` milliseconds. */
export function staleHole(budget: number): Hole {
	return { error: new StreamError(StreamCode.Old, { message: "group is past the budget" }), stale: budget };
}

/** A run of keys given up, with why. */
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
 * one key per DROP cannot grow the runs.
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

/** The error `drops` gave `key` up with, if any. */
export function dropAt(drops: readonly DropRun[], key: bigint): Error | undefined {
	return drops.find((run) => run.start <= key && key < run.end)?.error;
}

/** The index of the first of sorted, disjoint `runs` ending past `key`. */
export function runsFrom(runs: readonly Run[], key: bigint): number {
	let lo = 0;
	let hi = runs.length;
	while (lo < hi) {
		const mid = (lo + hi) >>> 1;
		if (runs[mid].end <= key) lo = mid + 1;
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
	/** Keys `[start, end)` this cursor gave up without serving, and why. */
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
	/** The frame's index within the group. */
	index: number;
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
	/** Ask the serving copy for a new group as `ask`, a request nobody withdraws. */
	ask(ask: Ask): void;
	/** Request a new group from the serving copy; see `Subscriber.refresh`. */
	refresh(signal?: AbortSignal): Promise<void>;
	/**
	 * The next group not in `delivered`, in arrival or sequence order, without waiting. With
	 * `dispositions`, a run no copy may still deliver is reported once as a `drop`.
	 */
	tryGroup(ordered: boolean, delivered: Delivered, dispositions: boolean): Recv;
	/**
	 * The next run no copy may still deliver, reported once as a `drop`, without taking a group.
	 * Below `passing`, which a key-order read is about to pass, no copy can deliver one later.
	 */
	tryDrop(delivered: Delivered, passing?: bigint): (Recv & { kind: "drop" }) | undefined;
	/**
	 * Whether the reader's bounded window has nothing left to deliver, judged by the serving copy
	 * against what it and the logical reader (`delivered`) handed out. A copy that may still
	 * deliver keeps it open, and an `ordered` reader passes what the copy's own publisher
	 * created past. Mirrors the Rust `Reader::exhausted`.
	 */
	exhausted(delivered: Delivered, ordered: boolean): boolean;
	/** Wake once a read may go differently; fires at most once. */
	changed(fn: () => void): Dispose;
	/** Forget the gaps in `delivered` below each group past the budget, judged on the newest copy. */
	forgetPastBudget(delivered: Delivered): void;
	/** The newest key any copy has seen. */
	latest(): bigint | undefined;
	/** The serving copy's declared start, raised to its cursor floor, or `"pending"` until resolved. */
	start(): bigint | "pending" | undefined;
	/** The logical track's declared exclusive end, as the first copy to know it declared it. */
	end(): Position | undefined;
	/** The longest run of keys just below `key` any copy knows were never produced. */
	skippedBelow(key: bigint): bigint;
	/** Where each range starts on the serving copy once delivery starts at `from`, undefined without one. */
	starts(from: bigint): (bigint | undefined)[] | undefined;
	/** Each range as the reader's window resolved it, whichever copy serves. */
	resolved(): (Resolved | undefined)[];
	/** The runs of `[start, end)` the serving copy reads, undefined without one. */
	runs(start: bigint, end: bigint): Run[] | undefined;
	/** Whether a time start the serving copy still waits to place holds back a gap; see `hooks.gapHeld`. */
	gapHeld(at: bigint, end: bigint): boolean;
	/** Whether a route's copy serves the reader now. */
	serving(): boolean;
	/** Where the run of keys from `key` the serving copy says no group arrives at ends; see `hooks.holeEnd`. */
	holeEnd(key: bigint, dropped: boolean): bigint | undefined;
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
	/** Whether `position` is the serving copy's largest and its group ended there. */
	largestFinished(position: Position): boolean;
	/**
	 * The serving copy's largest location once within the reader's cursor and newer than `seen`,
	 * or undefined once the logical track ends.
	 */
	largest(seen: Position | undefined): Promise<Position | undefined>;
	/** An independent subscriber to the serving copy. */
	fork(options?: Subscription): Subscriber;
	/** A fresh reader for a group still cached by one of this reader's copies. */
	cachedGroup(key: bigint, from?: number): GroupConsumer | undefined;
	/** As {@link cachedGroup} from its first frame, judged by the budget of the copy it comes from. */
	judgedGroup(key: bigint): GroupConsumer | undefined;
	/** When the latest group any copy caches in `[start, end)` landed; see `hooks.landed`. */
	landed(start: bigint, end: bigint): Instant | undefined;
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
export function finalKey(end: Position): bigint {
	return end.group + (end.frame === 0 ? 0n : 1n);
}

/** Hooks assigned in static blocks by the owning class. */
export const hooks: {
	/** Make a request for an unanswered datagram track. */
	datagramRequest: (producer: datagram.Producer, pending: Set<datagram.Request>) => datagram.Request;
	/** End a received datagram track if its last consumer left. */
	closeUnusedDatagrams: (producer: datagram.Producer) => boolean;
	/** A datagram track's properties, once accepted. */
	datagramInfo: (producer: datagram.Producer) => datagram.Info | undefined;
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
	/**
	 * Settles once the producer is released (closed or aborted), unlike its public `closed`,
	 * which also settles on a reached end while the producer keeps serving. Assigned by `track.ts`.
	 */
	trackReleased: (producer: Producer) => GetPromise<Error | null>;
	/** {@link trackReleased} for a demand handle. Assigned by `track.ts`. */
	demandReleased: (demand: Demand) => GetPromise<Error | null>;
	/**
	 * The group start subscribers ask for while it is past every key written, whether or not
	 * anyone subscribes now: what a relay forwards upstream as SUBSCRIBE_UPDATE's Wanted Start.
	 * Mirrors the Rust `Demand::asked`; assigned by `track.ts`.
	 */
	demandAsked: (demand: Demand) => Getter<bigint | undefined>;
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
	 * `Producer.skipGroups`, returning the cached keys it left alone, which a skip naming a
	 * group that arrived contradicts. Mirrors the Rust `skip_received`; assigned by `track.ts`.
	 */
	skipTrackGroups: (producer: Producer, start: bigint, end: bigint) => Run[];
	/**
	 * Append a group one past every group the track holds, or at 0 on an empty track: for tests
	 * and benches that count groups rather than time them. Mirrors the Rust `append_next`.
	 */
	appendNext: (producer: Producer) => GroupProducer;
	/**
	 * Forget every run a received copy holds as stale: a new upstream subscription opened, whose
	 * terms are new, so those groups are unknown again. Mirrors the Rust `supersede_stale`.
	 */
	supersedeStale: (producer: Producer) => void;
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
	receiveGroup: (producer: Producer, group: { start: bigint; offset: number; fill?: boolean }) => GroupProducer;
	/**
	 * Hold the copy of `key` still arriving, if one is, for a duplicate to wait on: until
	 * released, that copy's failure leaves its readers open for the duplicate to continue. Assigned
	 * by `track.ts`.
	 */
	arrivingGroup: (producer: Producer, key: bigint) => Arriving | undefined;
	/** The cached groups of `[start, end)` still arriving, by key, costing what the cache holds. Assigned by `track.ts`. */
	arrivingIn: (producer: Producer, start: bigint, end: bigint) => bigint[];
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
	/** What this subscriber resolves its ranges against, for a test. Assigned by `track.ts`. */
	subscriberHoldings: (subscriber: Subscriber) => Holdings;
	/** The times a received copy's feed accounting may place, for a test. Assigned by `track.ts`. */
	landingSpan: (producer: Producer) => Run | undefined;
	/**
	 * How many keys just below `key` the publisher declared it never produced: the skip run
	 * ending at `key`, which a moq-transport Prior Group ID Gap names. Mirrors the Rust
	 * `skipped_below`; assigned by `track.ts`.
	 */
	skippedBelow: (subscriber: Subscriber, key: bigint) => bigint;
	/** Ask a producer for a new group, judged by its {@link Info.refresh}; mirrors the Rust `TrackState::ask`. */
	askProducer: (producer: Producer, ask: Ask) => void;
	/**
	 * {@link askProducer} for a `Subscriber.refresh` caller, resolving once the request is sent.
	 * Aborting `signal` first withdraws it. Assigned by `track.ts`.
	 */
	requestProducer: (producer: Producer, signal?: AbortSignal) => Promise<void>;
	/** Ask the copy a subscriber reads for a new group as `ask`, a request nobody withdraws. Assigned by `track.ts`. */
	askSubscriber: (subscriber: Subscriber, ask: Ask) => void;

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
	/**
	 * Have a received copy's readers start a time anchor at the group starting there, held or
	 * not, for a wire whose publisher starts one at the first group at or after it. Mirrors the
	 * Rust `track::Producer::starts_at_keys`; assigned by `track.ts`.
	 */
	startsAtKeys: (producer: Producer) => void;
	/**
	 * Record where the upstream publisher started each of a lite-07 copy's live wire ranges, as
	 * `[time, start, through]` by the time each asked from (0 for the oldest group), `start`
	 * undefined for one with no group to deliver, replacing the last set: a reader's time start the
	 * copy cannot place among its own groups resolves to it. An empty answer holds for a reader
	 * within `budget`, the narrowest the subscription asked in milliseconds. Mirrors the Rust
	 * `track::Producer::time_starts`; assigned by `track.ts`.
	 */
	timeStarts: (
		producer: Producer,
		starts: Iterable<readonly [bigint, bigint | undefined, (bigint | undefined)?]>,
		budget: number,
	) => void;
	/**
	 * Tell a lite-07 copy where its publisher started each live range counting back, as
	 * `[back, start, latest]` triples, replacing the last set: the copy holds only part of the
	 * track, so a reader's relative start counts back as the publisher did, until a group past
	 * the `latest` it counted from moves the live edge. Assigned by `track.ts`.
	 */
	relativeStarts: (producer: Producer, starts: Iterable<readonly [bigint, bigint, bigint | undefined]>) => void;
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
	/**
	 * Seed a received track from the publisher's reported edge, and whether its group ended there;
	 * assigned by `track.ts`.
	 */
	reportedLargest: (producer: Producer, largest: Position | undefined, finished: boolean) => void;
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
	/** Snapshot the latest group key for wire positioning; assigned by `track.ts`. */
	latestKey: (subscriber: Subscriber) => bigint | undefined;
	/** How many groups arrived on the subscriber's copy so far; assigned by `track.ts`. */
	arrived: (subscriber: Subscriber) => number;
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
	 * Whether a time start still waiting below `end` holds back a gap an ordered read at `at` would
	 * return. Assigned by `track.ts`.
	 */
	gapHeld: (subscriber: Subscriber, at: bigint, end: bigint) => boolean;
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
	/**
	 * A subscriber's ranges keyed `keys` as resolved where they are served, as
	 * {@link servingResolved} reads them, undefined for one not held. Asked together, so a reader
	 * resolving by position on route copies builds its ranges once for them all. Assigned by
	 * `track.ts`.
	 */
	servingResolvedKeys: (subscriber: Subscriber, keys: readonly bigint[]) => (Resolved | undefined)[];
	/**
	 * Edit a subscriber's preferences by key, costing the ranges the edit names rather than every
	 * range: lite-07's SUBSCRIBE_UPDATE, by Range ID. Mirrors the Rust `Subscriber::edit`. Assigned
	 * by `track.ts`.
	 */
	editSubscriber: (subscriber: Subscriber, edit: Edit) => void;
	/**
	 * A subscriber's preferences, whose scalars and revision read without materializing every
	 * range as {@link Subscriber.subscription} does. Assigned by `track.ts`.
	 */
	preferences: (subscriber: Subscriber) => Prefs;
	/**
	 * `subscription`'s ranges resolved against the subscriber's track now, by index. Mirrors the
	 * Rust `Subscriber::resolve_ranges`. Assigned by `track.ts`.
	 */
	resolveRanges: (subscriber: Subscriber, subscription: Subscription) => (Resolved | undefined)[];
	/** The lowest buffered candidate, which an ordered take may wait below; `floor` passes keys taken on other copies. */
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
	 * out `delivered`, counting what the copy handed out itself, read in key order when
	 * `ordered`. Mirrors the Rust `exhausted_for`.
	 */
	exhaustedFor: (subscriber: Subscriber, delivered: Delivered, ordered: boolean) => boolean;
	/** How many runs of handed-out groups a subscriber remembers: one per gap, plus the floor. Tests only. */
	deliveredRuns: (subscriber: Subscriber) => number;
	/** How many runs a subscriber's record of what it reported holds. Tests only. */
	reportedRuns: (subscriber: Subscriber) => number;
	/** Whether the subscriber already handed out group `key`, or no longer expects it. */
	handedOut: (subscriber: Subscriber, key: bigint) => boolean;
	/** Forget the gaps in another reader's `delivered` below each group past this subscriber's budget. */
	forgetPastBudget: (subscriber: Subscriber, delivered: Delivered) => void;
	/** Fold a route copy's record below `at`, where the reader it feeds is, which reports nothing there. */
	fold: (subscriber: Subscriber, at: bigint) => void;
	/**
	 * What a lite publisher told its wire about a subscriber's keys, kept in the
	 * subscriber's own record. Mirrors the Rust crate-private `track::Subscriber::record_mut`.
	 */
	wire: (subscriber: Subscriber) => WireRecord;
	/**
	 * Whether this subscriber's budget, or `budget` in milliseconds when given, gives up on `key`,
	 * judged by its successor; see {@link expireGroup}.
	 */
	staleAt: (subscriber: Subscriber, key: bigint, budget?: number) => Expiry;
	/** A group the subscriber's track still caches, as a fresh read handle. */
	cachedGroup: (subscriber: Subscriber, key: bigint, from?: number) => GroupConsumer | undefined;
	/**
	 * As {@link cachedGroup} from its first frame, the reader given up once the subscription's
	 * budget gives up on it, judged by the route copy it is read from when routed.
	 */
	judgedGroup: (subscriber: Subscriber, key: bigint) => GroupConsumer | undefined;
	/** Whether {@link cachedGroup} would find the group, without making a read handle. */
	holdsGroup: (subscriber: Subscriber, key: bigint) => boolean;
	/**
	 * When the latest group the subscriber's track caches in `[start, end)` landed, undefined when
	 * it caches none; a routed reader asks every copy. Mirrors the Rust `Subscriber::landed`.
	 */
	landed: (subscriber: Subscriber, start: bigint, end: bigint) => Instant | undefined;
	/**
	 * The start of the group before `start` on the subscriber's track, as a lite-07 GROUP's
	 * Previous Start names it: just below the run of starts declared skipped directly under
	 * `start`, else `start - 1` when a group starts there, else undefined. Mirrors the Rust
	 * crate-private `track::Subscriber::previous_start`.
	 */
	previousStart: (subscriber: Subscriber, start: bigint) => bigint | undefined;
	/**
	 * Where the run of keys from `key` that no group can arrive at on the subscriber's track ends:
	 * a skip, or with `dropped` a drop given up for good. Mirrors the Rust crate-private
	 * `track::Subscriber::hole_end`.
	 */
	holeEnd: (subscriber: Subscriber, key: bigint, dropped: boolean) => bigint | undefined;
	/** The error the subscriber's track gave `key` up with, if it did, and what changes it. */
	givenUp: (subscriber: Subscriber, key: bigint) => { error?: Error; changed: Getter<unknown>[] };
	/**
	 * The parts of `[start, end)` the subscriber's track still holds as a received stale run: one
	 * a new upstream subscription superseded is gone. Mirrors the Rust crate-private
	 * `track::Subscriber::stale_held`.
	 */
	staleHeld: (subscriber: Subscriber, start: bigint, end: bigint) => Run[];
	/** Whether a group may still land on the live feed of the subscriber's track; see {@link Land}. */
	mayLand: (subscriber: Subscriber, land: Land) => boolean;
	/** A subscriber's largest location now, and whether it can still advance. */
	peekLargest: (subscriber: Subscriber) => { largest?: Position; ended: boolean };
	/** Whether `position` is a subscriber's largest and its group ended there, so no frame follows it. */
	largestFinished: (subscriber: Subscriber, position: Position) => boolean;
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
	/** Apply a FETCH reader budget without inheriting SUBSCRIBE's zero default, and the frame it asks from. */
	fetchBudget: (subscriber: Subscriber, maxDelay: Duration | undefined, frameStart?: number) => void;
	/** Whether a reader beyond one-shot fetches subscribes to this copy. */
	subscribing: (producer: Producer) => Getter<boolean>;
	/** The aggregate across every reader, one-shot fetches included: what the wire asks for. */
	requested: (producer: Producer) => Getter<Subscription | undefined>;
	/** Leave a wire copy serving its cache and fills without a live feed. */
	cacheTrack: (producer: Producer) => void;
	/** Fail subscriptions reaching beyond `edge`, keeping history readers and one-shot fetches. */
	refuseLive: (producer: Producer, edge: bigint, error: Error) => void;
	/**
	 * A fetch's group `key`, before it reads: `true` once it is cached or asked of a fill,
	 * written yet or not, as the Rust `fetch_group` queues one; `false` with no handler to ask;
	 * `"pending"` while the track awaits its accept, which opens its handler.
	 */
	askFetch: (subscriber: Subscriber, key: bigint) => boolean | "pending";
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
	/** Whether this source copy can no longer serve frame 0: it began past it, or shed it. */
	groupHeadless: (group: GroupProducer) => boolean;
	writeGroupFrame: (group: GroupProducer, frame: Frame, inflated?: number) => void;
	/** Payload footprint including a shared inflated copy, for measurements. */
	groupCacheBytes: (group: GroupProducer) => number;
	/** Record a cached group's accesses to `charge` from now on, or stop with undefined. */
	chargeGroup: (group: GroupProducer, charge: Charge | undefined) => void;
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
	/** Settles once the reader releases the group (`close` or an abort), however far it read. */
	groupReleased: (group: GroupConsumer) => Promise<void>;
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
	/** The first frame this copy begins at: past 0 for one missing its head. */
	groupFirstFrame: (group: GroupConsumer) => number;
	/** Watch the error a group's copy closes with, ignoring readers' verdicts, without holding the consumer. */
	groupFate: (group: GroupConsumer) => () => Error | undefined;
	/** Record when a track's mirror of a group joined that track at this hop. */
	arriveGroup: (group: GroupConsumer, at: Instant) => void;
	/** Keep applying a subscription's drift policy after it hands a group out. */
	expireGroup: (group: GroupConsumer, expiry: Expiry) => void;
	/** The drift policy a group carries, if any; see {@link expireGroup}. */
	groupExpiry: (group: GroupConsumer) => Expiry | undefined;
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
	 * Fails those `next` cannot serve. Assigned by `group.ts`.
	 */
	succeedGroup: (next: GroupProducer, failed: GroupProducer) => void;
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
	datagramInfo: () => {
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
	trackReleased: () => {
		throw new Error("track.ts not loaded");
	},
	demandReleased: () => {
		throw new Error("track.ts not loaded");
	},
	demandAsked: () => {
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
	appendNext: () => {
		throw new Error("track.ts not loaded");
	},
	producerLargest: () => {
		throw new Error("track.ts not loaded");
	},
	askProducer: () => {
		throw new Error("track.ts not loaded");
	},
	requestProducer: () => {
		throw new Error("track.ts not loaded");
	},
	askSubscriber: () => {
		throw new Error("track.ts not loaded");
	},
	subscriberEnd: () => {
		throw new Error("track.ts not loaded");
	},
	subscriberEndError: () => {
		throw new Error("track.ts not loaded");
	},
	subscriberHoldings: () => {
		throw new Error("track.ts not loaded");
	},
	landingSpan: () => {
		throw new Error("track.ts not loaded");
	},
	skippedBelow: () => {
		throw new Error("track.ts not loaded");
	},
	startTrack: () => {
		throw new Error("track.ts not loaded");
	},
	dropAnchors: () => {
		throw new Error("track.ts not loaded");
	},
	startsAtKeys: () => {
		throw new Error("track.ts not loaded");
	},
	timeStarts: () => {
		throw new Error("track.ts not loaded");
	},
	relativeStarts: () => {
		throw new Error("track.ts not loaded");
	},
	servingRuns: () => {
		throw new Error("track.ts not loaded");
	},
	gapHeld: () => {
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
	servingResolvedKeys: () => {
		throw new Error("track.ts not loaded");
	},
	editSubscriber: () => {
		throw new Error("track.ts not loaded");
	},
	preferences: () => {
		throw new Error("track.ts not loaded");
	},
	resolveRanges: () => {
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
	latestKey: () => {
		throw new Error("track.ts not loaded");
	},
	arrived: () => {
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
	fold: () => {
		throw new Error("track.ts not loaded");
	},
	wire: () => {
		throw new Error("track.ts not loaded");
	},
	supersedeStale: () => {
		throw new Error("track.ts not loaded");
	},
	deliveredRuns: () => {
		throw new Error("track.ts not loaded");
	},
	reportedRuns: () => {
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
	judgedGroup: () => {
		throw new Error("track.ts not loaded");
	},
	holdsGroup: () => {
		throw new Error("track.ts not loaded");
	},
	landed: () => {
		throw new Error("track.ts not loaded");
	},
	previousStart: () => {
		throw new Error("track.ts not loaded");
	},
	holeEnd: () => {
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
	largestFinished: () => {
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
	arrivingIn: () => {
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
	groupHeadless: () => {
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
	groupReleased: () => {
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
	groupFirstFrame: () => {
		throw new Error("group.ts not loaded");
	},
	groupFate: () => {
		throw new Error("group.ts not loaded");
	},
	expireGroup: () => {
		throw new Error("group.ts not loaded");
	},
	groupExpiry: () => {
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

/** The Spread Hash's offset basis, which lite-06 and earlier rank by. */
const SPREAD_BASIS = 0x420c0decb00bn;

/** FNV-1a 64 from `basis` of `path`, then `bytes`. */
function fnv(path: string, bytes: Iterable<number>, basis = SPREAD_BASIS): bigint {
	const prime = 0x100000001b3n;
	let hash = basis;
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
 * The draft's Rendezvous Hash: standard FNV-1a 64 of `path` then the instance's UTF-8, which
 * spreads a pool's lite-07 claims across paths the same on every node. Mirrors `rendezvous` in
 * `rs/moq-net`.
 */
function rendezvousHash(path: string, instance: string): bigint {
	return fnv(path, new TextEncoder().encode(instance), 0xcbf29ce484222325n);
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

/**
 * What `pick` finds at the most specific claim covering `path`: it picks at each covering prefix,
 * deepest first, and a claim shown above its own prefix counts for the shallower prefix it is
 * really at. Of claims as specific as each other the deepest prefix wins. Mirrors
 * `RouteTable::best_route` in rs/moq-net.
 */
export function mostSpecific<T>(
	path: Path.Valid,
	pick: (prefix: Path.Valid) => T | undefined,
	route: (found: T) => WireRoute,
): T | undefined {
	let best: T | undefined;
	let bestSpecificity = -1;
	let depth = Path.parts(path).length;
	for (const prefix of coveringPrefixes(path)) {
		const found = pick(prefix);
		const specificity = found === undefined ? 0 : depth - Number(route(found).above ?? 0n);
		if (found !== undefined && specificity > bestSpecificity) {
			best = found;
			bestSpecificity = specificity;
		}
		depth--;
	}
	return best;
}

/** Covering prefixes, most specific first, including the empty root. */
export function* coveringPrefixes(path: Path.Valid): Generator<Path.Valid> {
	const segments = Path.parts(path);
	let end = path.length;
	for (let i = segments.length; ; i--) {
		yield path.slice(0, end) as Path.Valid;
		if (i === 0) return;
		// Drop the last segment and the `/` before it, if any.
		end = Math.max(end - segments[i - 1].length - 1, 0);
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
 * The highest epoch each instance reaches among a set of routes, per claim: a claim is its prefix
 * and how far above it was shown, so a broader claim shown at a prefix is another claim than the
 * one made there, and its epoch supersedes nothing of it. Built in one pass so ranking N routes
 * stays linear. Only epochs above 0 are kept, since nothing is below 0: the common case, where
 * every instance announces epoch 0, stores nothing. Mirrors `Epochs` in rs/moq-net, whose routes
 * are all of one prefix here.
 */
export class Epochs {
	readonly #highest = new Map<Instance.Valid | undefined, Map<bigint, bigint>>();

	constructor(routes: Iterable<WireRoute>) {
		for (const route of routes) {
			const epoch = route.epoch ?? 0n;
			if (epoch === 0n) continue;
			let above = this.#highest.get(route.instance);
			if (!above) {
				above = new Map();
				this.#highest.set(route.instance, above);
			}
			const highest = above.get(route.above ?? 0n);
			if (highest === undefined || epoch > highest) above.set(route.above ?? 0n, epoch);
		}
	}

	/** Whether a route carries `route`'s instance at a higher epoch on the same claim. */
	supersedes(route: WireRoute): boolean {
		const highest = this.#highest.get(route.instance)?.get(route.above ?? 0n);
		return highest !== undefined && highest > (route.epoch ?? 0n);
	}
}

/**
 * Prefer a route that is neither draining (a peer that sent GOAWAY) nor unavailable (held),
 * which rank below every other, then the highest priority on its path, then identified
 * publishers, then lower static cost.
 */
export function compareRoutes(a: WireRoute, b: WireRoute): number {
	const last = (route: WireRoute) => Number((route.draining ?? false) || route.available === false);
	return (
		last(a) - last(b) ||
		(b.priority ?? 0) - (a.priority ?? 0) ||
		Number(isAnonymous(a)) - Number(isAnonymous(b)) ||
		(a.cost < b.cost ? -1 : a.cost > b.cost ? 1 : 0)
	);
}

/**
 * Order eligible routes at `prefix` resolving `path` by preference, hop count, then the spread
 * hash of each instance, then, within one instance, the hash of its chain. A lite-07 claim counts
 * as one hop, to the peer that sent it, and ranks by the rendezvous hash of its instance. The
 * hashes only spread a prefix's paths: at the prefix itself a tie is left for the newest to win
 * (see {@link bestCandidate}), so a publisher that restarts under a new instance takes over from
 * its lingering session at once, matching `route_order` in rs/moq-net for routes a node did not
 * get from a cluster peer, which is every route here.
 */
export function compareRouteCandidates(path: Path.Valid, prefix: Path.Valid, a: WireRoute, b: WireRoute): number {
	// The most specific claim wins outright: an exact claim shadows broader ones shown at its path.
	const above = (a.above ?? 0n) - (b.above ?? 0n);
	const order = (above < 0n ? -1 : above > 0n ? 1 : 0) || compareRoutes(a, b) || routeHops(a) - routeHops(b);
	if (order !== 0 || path === prefix) return order;
	const [a1, a2] = routeKey(path, a);
	const [b1, b2] = routeKey(path, b);
	if (a1 !== b1) return a1 < b1 ? -1 : 1;
	return a2 < b2 ? -1 : a2 > b2 ? 1 : 0;
}

/** The hops a route ranks by: a lite-07 claim counts as the one hop to the peer that sent it. */
export function routeHops(route: WireRoute): number {
	return route.claimed ? 1 : route.hops.length;
}

/** The hash pair a route ranks by beneath its prefix, each route keyed on its own: a lite-07 claim
 * by the rendezvous hash of its instance, any other by the spread hash of its instance and then
 * the hash of its chain. As `route_order` in rs/moq-net. */
function routeKey(path: Path.Valid, route: WireRoute): [bigint, bigint] {
	if (route.claimed) return [rendezvousHash(path, route.instance ? Instance.text(route.instance) : ""), 0n];
	return [spreadHash(path, route.instance ? Instance.text(route.instance) : ""), chainHash(path, route.hops)];
}

/**
 * A subscriber's own ranges, resolved to the group starts it reads, and the helpers that
 * translate ranges between the model and the wire. Mirrors the Rust `subscription` module.
 *
 * The aggregate a publisher sees is the union of every subscriber's ranges, so it says what
 * arrives, not what one subscriber asked for. A {@link Window} is the per-subscriber side:
 * resolved at the hop serving the subscriber, it filters that subscriber's reads.
 *
 * @module
 */

import type { Duration } from "@moq/clock";
import type { Anchor, Range } from "./track.ts";
import { max, min, UNBOUNDED } from "./util/key.ts";

/** A run of group starts `[start, end)`; an open run ends at {@link UNBOUNDED}. */
export type Run = { start: bigint; end: bigint };

/**
 * One range resolved to groups: from `start` up to the exclusive `end`, undefined while open.
 * `floored` says whether the start names a group, rather than reaching back to the oldest one.
 */
export type Resolved = { start: bigint; end?: bigint; floored: boolean };

/** Everything from the latest group onward. */
export const LIVE: Range = Object.freeze({ start: Object.freeze({ relative: 0n }) });

/** Check one anchor's shape, naming `field` in the error. */
function anchorOf(field: string, anchor: Anchor): Anchor {
	const keys = typeof anchor === "object" && anchor !== null ? Object.keys(anchor) : [];
	const value = keys.length === 1 ? (anchor as Record<string, unknown>)[keys[0]] : undefined;
	if (keys[0] === "time" && typeof value === "bigint" && value >= 0n) return { time: value };
	if (keys[0] === "relative" && typeof value === "bigint" && value >= 0n) return { relative: value };
	throw new TypeError(`${field} must be {relative} or {time}, a non-negative bigint`);
}

/** Check and copy a caller's ranges, refusing a malformed one rather than serving it widened. */
export function rangesOf(ranges: readonly Range[]): Range[] {
	if (!Array.isArray(ranges)) throw new TypeError("ranges must be an array");
	return ranges.map((range) => {
		if (typeof range !== "object" || range === null) throw new TypeError("a range must be an object");
		const out: Range = {};
		if (range.start !== undefined) out.start = anchorOf("range start", range.start);
		if (range.end !== undefined) out.end = anchorOf("range end", range.end);
		return out;
	});
}

/** The group start a time anchor names, or undefined for any other anchor. */
function time(anchor: Anchor | undefined): bigint | undefined {
	return anchor?.time;
}

/** How many groups back a relative anchor counts, or undefined for any other anchor. */
function relative(anchor: Anchor | undefined): bigint | undefined {
	return anchor?.relative;
}

/**
 * The range from group `start` up to the exclusive group `end`, or onward for undefined: what a
 * wire's group bounds become. Undefined when it holds nothing, which an exclusive end of 0 says
 * whatever the start. Mirrors the Rust `Range::span`.
 */
export function span(start: bigint | undefined, end: bigint | undefined): Range | undefined {
	if (end === 0n) return undefined;
	const range: Range = {};
	if (start !== undefined) range.start = { time: start };
	if (end !== undefined) range.end = { time: end - 1n };
	return isEmpty(range) ? undefined : range;
}

/** Whether `range` certainly delivers nothing, wherever the latest group lands. */
export function isEmpty(range: Range): boolean {
	const start = time(range.start);
	const end = time(range.end);
	return start !== undefined && end !== undefined && end < start;
}

/** An anchor as a sort key: relative, then `none` for an omitted one, then time. */
function anchorKey(anchor: Anchor | undefined, none: [number, bigint]): [number, bigint] {
	if (anchor === undefined) return none;
	if (anchor.relative !== undefined) return [0, anchor.relative];
	return [2, anchor.time];
}

/**
 * A canonical order, as the Rust `Range` sorts: by start, relative ones first, then the oldest,
 * then time ones; then by end, open first.
 */
function compare(a: Range, b: Range): number {
	const [x, y] = [anchorKey(a.start, [1, 0n]), anchorKey(b.start, [1, 0n])];
	const [p, q] = [anchorKey(a.end, [-1, 0n]), anchorKey(b.end, [-1, 0n])];
	return x[0] - y[0] || order(x[1], y[1]) || p[0] - q[0] || order(p[1], q[1]);
}

// Compare a group start or a count, which only ever meet their own kind.
function order(a: bigint, b: bigint): number {
	return a < b ? -1 : a > b ? 1 : 0;
}

/** Whether two lists hold the same ranges in the same order. */
export function sameRanges(a: readonly Range[], b: readonly Range[]): boolean {
	return a.length === b.length && a.every((range, index) => rangeKey(range) === rangeKey(b[index]));
}

// A range as a string, equal exactly when the ranges are; JSON can't hold a bigint.
function rangeKey(range: Range): string {
	return `${anchorText(range.start)}..${anchorText(range.end)}`;
}

function anchorText(anchor: Anchor | undefined): string {
	if (anchor === undefined) return "";
	if (anchor.time !== undefined) return `t${anchor.time}`;
	return `r${anchor.relative}`;
}

/**
 * The union of `ranges` in a canonical order, without empty or redundant ranges.
 *
 * Time spans merge where they overlap or touch, as the group starts they cover. The rest can't be
 * placed without the latest group, so only an open range drops what it certainly covers: the open
 * time span drops ranges starting inside it, and the farthest open relative range drops relative
 * ranges starting no farther back. The oldest start covers everything. Mirrors the Rust `union`.
 */
export function union(ranges: readonly Range[]): Range[] {
	const kept = ranges.filter((range) => !isEmpty(range));
	if (kept.some((range) => range.start === undefined && range.end === undefined)) return [{}];

	const spans: Run[] = [];
	for (const range of kept) {
		const start = time(range.start);
		if (start === undefined) continue;
		if (range.end === undefined) spans.push({ start, end: UNBOUNDED });
		else {
			const end = time(range.end);
			if (end !== undefined) spans.push({ start, end: end + 1n });
		}
	}
	spans.sort((a, b) => order(a.start, b.start) || order(a.end, b.end));
	const runs: Run[] = [];
	for (const span of spans) {
		const last = runs.at(-1);
		if (last && span.start <= last.end) last.end = max(last.end, span.end);
		else runs.push({ ...span });
	}
	const tail = runs.at(-1)?.end === UNBOUNDED ? runs.at(-1)?.start : undefined;
	let farthest: bigint | undefined;
	for (const range of kept) {
		const back = relative(range.start);
		if (back !== undefined && range.end === undefined) farthest = max(farthest ?? back, back);
	}

	const out: Range[] = runs.map((run) =>
		run.end === UNBOUNDED
			? { start: { time: run.start } }
			: { start: { time: run.start }, end: { time: run.end - 1n } },
	);
	for (const range of kept) {
		const start = time(range.start);
		const back = relative(range.start);
		if (start !== undefined) {
			// Merged above unless it ends relative to the latest group.
			if (range.end === undefined || time(range.end) !== undefined) continue;
			if (tail === undefined || start < tail) out.push(range);
		} else if (back !== undefined) {
			if (range.end === undefined) continue;
			if (farthest === undefined || back > farthest) out.push(range);
		} else out.push(range);
	}
	if (farthest !== undefined) out.push({ start: { relative: farthest } });

	out.sort(compare);
	return out.filter((range, index) => index === 0 || rangeKey(range) !== rangeKey(out[index - 1]));
}

/**
 * The lowest group any range names by time, or undefined when one counts back from the latest
 * group or reaches back to the oldest one: the floor a wire without ranges carries. Mirrors the
 * Rust `Subscription::start`.
 */
export function rangesStart(ranges: readonly Range[]): bigint | undefined {
	let floor: bigint | undefined;
	for (const range of ranges) {
		if (isEmpty(range)) continue;
		const start = time(range.start);
		if (start === undefined) return undefined;
		floor = min(floor ?? start, start);
	}
	return floor;
}

/**
 * The exclusive end past every range, or undefined when one is open or ends relative to the
 * latest group: the cap a wire without ranges carries. No range at all ends at group 0. Mirrors
 * the Rust `Subscription::end`.
 */
export function rangesEnd(ranges: readonly Range[]): bigint | undefined {
	let end = 0n;
	for (const range of ranges) {
		if (isEmpty(range)) continue;
		const last = time(range.end);
		if (last === undefined) return undefined;
		end = max(end, last + 1n);
	}
	return end;
}

/**
 * Whether a wire without ranges carries `range` upstream: time anchors, as group IDs, and the
 * live and oldest starts it spells with no floor. Mirrors the Rust `Range::is_wire`.
 */
export function isWire(range: Range): boolean {
	const back = relative(range.start);
	return (back === undefined || back === 0n) && relative(range.end) === undefined;
}

/**
 * The ranges a SUBSCRIBE carries for `ranges`, without the empty ones. Mirrors the Rust
 * `wire_ranges`.
 *
 * The copy resolves a relative end itself, against the groups it receives past it, so it asks
 * through the live edge: SUBSCRIBE_START names a resolved start, but nothing names a resolved
 * end, so one resolved upstream would leave the copy short.
 */
export function wireRanges(ranges: readonly Range[]): Range[] {
	return ranges
		.filter((range) => !isEmpty(range))
		.map((range) =>
			range.end === undefined || time(range.end) !== undefined
				? range
				: range.start
					? { start: range.start }
					: {},
		);
}

/** Whether every range has an end, so a lite-07 publisher ends the subscription once it covers them. */
export function bounded(ranges: readonly Range[]): boolean {
	return ranges.every((range) => range.end !== undefined);
}

/**
 * Whether a subscriber joins at the live edge: a range counting back from the latest group (or
 * reaching back to the oldest) and following it. A fixed start resumes a known group instead,
 * such as a route switch picking up where the reader stopped. Mirrors the Rust
 * `Subscription::joins_live`.
 */
export function joinsLive(ranges: readonly Range[] | undefined): boolean {
	return (ranges ?? [{}]).some((range) => time(range.start) === undefined && range.end === undefined);
}

/**
 * The one span a wire without ranges carries for `ranges`, which the subscribers' own reads
 * narrow again: from the lowest time start up to {@link rangesEnd}.
 *
 * A subscription counting only back from the latest group asks with no start, which a pre-06
 * publisher reads as the latest group and a lite-06 one as no floor. One mixing the two asks
 * from group 0, since no single start covers both: the latest group may sit below the time
 * start. Mirrors the Rust `wire_span`.
 */
export function wireSpan(ranges: readonly Range[]): { start?: bigint; end?: bigint } {
	let start = rangesStart(ranges);
	if (start === undefined && ranges.some((range) => !isEmpty(range) && time(range.start) !== undefined)) start = 0n;
	return { start, end: rangesEnd(ranges) };
}

/** A range holding nothing: nothing sorts below group 0. Mirrors the Rust `Range::EMPTY`. */
export const EMPTY: Range = Object.freeze({ start: Object.freeze({ time: 1n }), end: Object.freeze({ time: 0n }) });

/**
 * `ranges` with every range raised to start no earlier than `floor` and stop before the exclusive
 * `end`, without ever widening one. Mirrors the Rust `Subscription::clamped`.
 *
 * `resolved` names where each range resolved, by index, so a relative or time anchor becomes its
 * own group, raised to `floor` or capped at `end`. A start not resolved keeps its anchor, since the
 * floor may lie before it, so the caller drops what it resolves to below `floor`. An end not
 * resolved becomes the cap.
 */
export function clamped(
	ranges: readonly Range[],
	floor: bigint | undefined,
	end: bigint | undefined,
	resolved: readonly (Resolved | undefined)[],
): Range[] {
	if (floor === undefined && end === undefined) return [...ranges];
	return ranges.map((range, index) => {
		const at = resolved[index];
		let start = at ? { time: at.start } : range.start;
		const named = time(start);
		if (floor !== undefined && named !== undefined) start = { time: max(named, floor) };
		let requested = range.end;
		if (requested !== undefined && at?.end !== undefined) {
			if (at.end === 0n) return EMPTY;
			requested = { time: at.end - 1n };
		}
		if (end !== undefined) {
			if (end === 0n) return EMPTY;
			const capped = time(requested);
			requested = { time: capped === undefined ? end - 1n : min(capped, end - 1n) };
		}
		const out: Range = {};
		if (start !== undefined) out.start = start;
		if (requested !== undefined) out.end = requested;
		return out;
	});
}

/** Where a time, or a count back from the latest group, falls among the groups a hop holds. Mirrors the Rust `Containing`. */
export type Containing =
	/** The newest group starting at or before it, or the group counted back to. */
	| { group: bigint }
	/** Every group held starts after it. */
	| "before"
	/** The groups held can't place it yet. */
	| "unknown";

/**
 * The groups a serving hop holds, by start, for resolving relative and time anchors. Mirrors the
 * Rust `Holdings`.
 */
export interface Holdings {
	/**
	 * Whether this hop resolves ranges against groups of its own. A hop that holds none, such as a
	 * reader following route copies, leaves its ranges to the copies.
	 */
	resolves(): boolean;
	/** Where `time` falls among the groups held. */
	containing(time: bigint): Containing;
	/** The lowest group held starting at or after `from`. */
	nextStart(from: bigint): bigint | undefined;
	/**
	 * The group `back` groups before `latest`, which counts as the first, among the groups held:
	 * undefined once fewer are held than that.
	 */
	back(latest: bigint, back: bigint): bigint | undefined;
	/**
	 * Where a range counting `back` groups before `latest` starts, which a relative start, but never
	 * an end, takes over {@link back}: where the publisher reported starting it, since a delivery
	 * start may sit past the group counted, such as above groups too stale for the budget, or
	 * `"unknown"` while a received copy can count it neither that way nor over its own groups yet.
	 * Undefined, or absent, counts it with {@link back}. Mirrors the Rust `Holdings::counted_start`.
	 */
	countedStart?(latest: bigint, back: bigint): Containing | undefined;
	/** Whether nothing more can arrive here, so a time start nothing contains reaches back to the oldest group rather than wait. */
	complete(): boolean;
	/**
	 * Whether a fill handler can look up a time start the groups held can't place in the track's
	 * history, as the Rust `Holdings::looks_up` says. Absent for none.
	 */
	looksUp?(): boolean;
	/**
	 * Where a live join holding `min` behind the live edge starts, `latest` being the latest group:
	 * a group held, never a raw time a copy would ask its upstream to fill. Undefined while nothing
	 * held can measure it, so the join waits for a group as a relative start does. Absent where the
	 * hop leaves its ranges to the copies it follows. Mirrors the Rust `Holdings::reach_back`.
	 */
	reachBack?(latest: bigint, min: Duration): { anchor?: Anchor } | undefined;
	/**
	 * Whether the producer declared every start in `[start, end)` skipped, so no group starts there
	 * even in the history a handler serves. Absent for none.
	 */
	neverProduced?(start: bigint, end: bigint): boolean;
	/**
	 * Runs of times {@link containing} can't place, so a window skips its time starts in them rather
	 * than try each again whenever the groups held change. Absent, or undefined, when it can't tell,
	 * so each is tried. Mirrors the Rust `Holdings::unplaced`.
	 */
	unplaced?(): Unplaced | undefined;
}

/**
 * The times a {@link Holdings} can't place: sorted, disjoint runs, apart from the times a placement
 * that the groups and drops held don't decide places. `mark` changes whenever such a placement may
 * have, so a window that sees it unchanged tries only the times outside `runs` again.
 */
export type Unplaced = { runs: readonly Run[]; mark: number };

/** A hop that holds no groups of its own, such as a reader following route copies that resolve its ranges for it. */
export const UNHELD: Holdings = Object.freeze({
	resolves: () => false,
	containing: () => "unknown" as const,
	nextStart: () => undefined,
	back: (latest: bigint, back: bigint) => (back === 0n ? latest : undefined),
	complete: () => true,
});

/**
 * A hop that holds no groups of its own but knows every time it is asked starts a group, such as
 * a publisher's reported start: a time anchor names the group starting there.
 */
export const AT_KEYS: Holdings = Object.freeze({
	...UNHELD,
	containing: (time: bigint) => ({ group: time }),
});

/** One non-empty range, resolved. */
type Span = {
	/** Which of the ranges it came from. */
	index: number;
	/** Where it starts: its range's start, or where a live join's Min Age reached it back to. */
	anchor: Anchor | undefined;
	start: bigint;
	/** The exclusive end, {@link UNBOUNDED} past every group. */
	end: bigint;
	/** Whether the start names a group, rather than reaching back to the oldest one. */
	floored: boolean;
	/**
	 * A time start the groups held can't place, looked up from the track's history: the span reads
	 * nothing until the lookup answers.
	 */
	looking?: Lookup;
	/** A time start the groups held can't place yet: the span reads only the groups from that time until it can. */
	waiting: boolean;
	/**
	 * A relative start counting back past every group held asks no miss below this: the oldest group
	 * held when it was placed, since which of the older ones it reaches is unknown.
	 */
	reach?: bigint;
};

/**
 * A time start being looked up: its time, and the exclusive end of the run below every group
 * held, which holds the group containing it if the track's history does. Mirrors the Rust
 * `Looking`.
 */
export interface Lookup {
	time: bigint;
	below: bigint;
}

/**
 * A subscriber's ranges resolved to the group starts it reads. Mirrors the Rust `Window`.
 *
 * A relative or time anchor resolves when the window is built, and again only when the ranges
 * are replaced. Before any group exists a relative anchor waits, reading everything, until
 * {@link settle} names the first one. A time start the groups held can't place yet waits too,
 * reading only the groups from its time meanwhile, since whichever group contains it starts at or
 * before it.
 */
export class Window {
	/** What these were resolved from, so only replacing the ranges resolves again. */
	readonly ranges: readonly Range[];
	#spans: Span[] = [];
	/** The groups the spans cover: merged, sorted, and disjoint. */
	#runs: Run[] = [];
	/** The part of {@link #runs} a miss may ask for: see {@link missable}. */
	#missable: Run[] = [];
	/** A relative anchor waits on a first group, so nothing is filtered yet. */
	#pending = false;
	/**
	 * The spans, by position, whose time start waits on a group arriving, by that time: a change to
	 * the groups held tries only those its {@link Holdings.unplaced} leaves out.
	 */
	#waiting = new ByTime();
	/** The {@link Unplaced.mark} every waiting span was last tried under. */
	#waited: number | undefined;
	/**
	 * The spans, by position, still looking up their time start, which read nothing meanwhile, by
	 * that time: kept apart so an answer finds them without visiting every span.
	 */
	#looking = new ByTime();
	/**
	 * The ranges, by index, of a live join whose reach back waits on the groups held to measure the
	 * live edge, so they read nothing yet: a group held is no reason to start at it.
	 */
	#reaching: number[] = [];
	/**
	 * The ranges, by index, whose relative start the groups held can't count back to yet (see
	 * {@link Holdings.countedStart}), so they read nothing yet: a group held may sit below where the
	 * publisher counted to.
	 */
	#counting: number[] = [];
	/** Nothing is left to resolve as groups arrive. */
	#settled = false;
	/**
	 * How many spans name each start, lowest first, and how many name none: see {@link start}.
	 * Counted so a read, and a span starting, costs no walk of the spans.
	 */
	#named: { start: bigint; count: number }[] = [];
	#unnamed = 0;
	/**
	 * The subscriber's Min Age, which reaches a live join back from the live edge. Only a pending
	 * window reads it, when it places its ranges, so a join already started stays where it started.
	 */
	min: Duration | undefined;

	/**
	 * Resolve `ranges`, counting relative anchors back from `latest` and time anchors from
	 * `holdings`, and reaching a live join back by `min` (see {@link joins}).
	 */
	constructor(ranges: readonly Range[], latest: bigint | undefined, holdings: Holdings = UNHELD, min?: Duration) {
		this.ranges = ranges;
		this.min = min;
		this.#place(latest, holdings);
	}

	/** Whether a relative anchor still waits on a first group. */
	get pending(): boolean {
		return this.#pending;
	}

	/** Whether nothing is left to resolve as groups arrive. */
	get settled(): boolean {
		return this.#settled;
	}

	/**
	 * Resolve what the groups now held settle: relative anchors once `latest` exists, and time
	 * starts once a group arrives. Returns whether the groups in range moved.
	 */
	settle(latest: bigint | undefined, holdings: Holdings = UNHELD): boolean {
		if (this.#pending) {
			if (latest === undefined) return false;
			this.#place(latest, holdings);
			return !this.#pending;
		}
		// The spans placed or started here, each reading at least what it did before, unless one now
		// looks up its start and reads nothing meanwhile.
		const moved: number[] = [];
		if (this.#reaching.length > 0) {
			const placed = this.#spans.length;
			const reaching = this.#reaching;
			this.#reaching = [];
			for (const index of reaching) this.#placeRange(index, latest, holdings);
			for (let position = placed; position < this.#spans.length; position++) moved.push(position);
		}
		// Counted from the latest group now, as the publisher counts from its own latest when the
		// range reaches it.
		if (this.#counting.length > 0 && latest !== undefined) {
			const placed = this.#spans.length;
			const counting = this.#counting;
			this.#counting = [];
			for (const index of counting) this.#placeRange(index, latest, holdings);
			for (let position = placed; position < this.#spans.length; position++) moved.push(position);
		}
		const shrank = this.#waiting.size > 0 && this.#startWaiting(holdings, moved);
		if (shrank) this.#rebuild();
		else if (moved.length === 0) return false;
		else this.#grow(moved);
		return true;
	}

	// Start the spans whose time start waits on a group, once one arrived, or once nothing can
	// arrive any more, adding each one started to `started`. Only the times the holdings may now
	// place are tried, so a change placing one of them costs that one, not every span still
	// waiting. Returns whether one started reads less than before: it looks up its start, reading
	// nothing meanwhile.
	#startWaiting(holdings: Holdings, started: number[]): boolean {
		const complete = holdings.complete();
		const unplaced = complete ? undefined : holdings.unplaced?.();
		let tried: Timed[];
		if (unplaced !== undefined && this.#waited === unplaced.mark) {
			tried = [];
			for (const gap of outside(unplaced.runs)) {
				for (const entry of this.#waiting.takeIn(gap.start, gap.end)) tried.push(entry);
			}
		} else {
			tried = this.#waiting.takeAll();
			this.#waited = unplaced?.mark;
		}
		let shrank = false;
		for (const entry of tried) {
			const containing = holdings.containing(entry.time);
			let start: bigint;
			let floored: boolean;
			if (typeof containing === "object") [start, floored] = [containing.group, true];
			// The group holding it is older than any held, or nothing can arrive.
			else if (containing === "before" || complete) [start, floored] = [0n, false];
			else {
				this.#waiting.put(entry);
				continue;
			}
			for (const position of entry.positions) {
				if (this.#startSpan(position, entry.time, start, floored, holdings)) shrank = true;
				started.push(position);
			}
		}
		return shrank;
	}

	// Start the span at `position`, whose time start `at` waited, at `start`: looked up where the
	// groups held can't place it, as when first resolved, and otherwise reading its groups. Returns
	// whether it looks its start up.
	#startSpan(position: number, at: bigint, start: bigint, floored: boolean, holdings: Holdings): boolean {
		const span = this.#spans[position];
		const asked = time(this.ranges[span.index].start) !== undefined;
		this.#unname(position);
		span.start = start;
		span.floored = floored;
		span.waiting = false;
		const below = asked ? unresolved(at, start, floored, holdings) : undefined;
		if (below !== undefined) {
			span.looking = { time: at, below };
			this.#looking.add(at, position);
		}
		this.#name(position);
		return below !== undefined;
	}

	/** Whether group `key` is in range. Everything is while a relative anchor waits. */
	contains(key: bigint): boolean {
		if (this.#pending) return true;
		const run = this.#runAt(key);
		return run !== undefined && key >= run.start;
	}

	/** The lowest group in range at or above `from`, undefined when none is. `from` itself while a relative anchor waits. */
	next(from: bigint): bigint | undefined {
		if (this.#pending) return from;
		const run = this.#runAt(from);
		return run === undefined ? undefined : max(run.start, from);
	}

	/** The lowest group in range, undefined while an anchor waits. Nothing in range floors at group 0. */
	floor(): bigint | undefined {
		if (this.#pending || this.#unplaced()) return undefined;
		return this.#runs[0]?.start ?? 0n;
	}

	/**
	 * The lowest group the ranges name, undefined while one reaches back to the oldest group or an
	 * anchor waits.
	 */
	start(): bigint | undefined {
		if (this.#pending || this.#reaching.length > 0 || this.#counting.length > 0 || this.#unnamed > 0)
			return undefined;
		return this.#named[0]?.start;
	}

	/** The exclusive end past every group in range, undefined when one stays open or an anchor waits. Nothing in range ends at 0. */
	end(): bigint | undefined {
		if (!this.#settled) return undefined;
		const end = this.#runs.at(-1)?.end ?? 0n;
		return end === UNBOUNDED ? undefined : end;
	}

	/** The runs in range from `from` up to the exclusive `end`, lowest first. */
	runs(from = 0n, end = UNBOUNDED): Run[] {
		if (from >= end) return [];
		if (this.#pending) return [{ start: from, end }];
		return intersect(this.#runs, from, end);
	}

	/**
	 * The part of {@link runs} a miss may ask a fill for: all of it but what starts before a time
	 * start that fell before every group held, or below the oldest group held when a relative start
	 * counting back past them all was placed. Mirrors the Rust `Window::missable`.
	 */
	missable(from = 0n, end = UNBOUNDED): Run[] {
		if (from >= end) return [];
		if (this.#pending) return [{ start: from, end }];
		return intersect(this.#missable, from, end);
	}

	/**
	 * Each range resolved, in the order given: undefined for one whose start still waits, or is
	 * still looked up. One empty as given ends at its start. Mirrors the Rust `Window::resolved`.
	 */
	resolved(): (Resolved | undefined)[] {
		const resolved: (Resolved | undefined)[] = this.ranges.map((range) => {
			const start = time(range.start);
			return start !== undefined && isEmpty(range) ? { start, end: start, floored: true } : undefined;
		});
		for (const span of this.#spans) {
			if (span.looking || span.waiting) continue;
			resolved[span.index] = {
				start: span.start,
				end: span.end === UNBOUNDED ? undefined : span.end,
				floored: span.floored,
			};
		}
		return resolved;
	}

	/**
	 * Where each range starts once delivery starts at `from`, in the order given: undefined for
	 * one with no group there, or whose start still waits. Mirrors the Rust `Window::starts`.
	 */
	starts(from: bigint): (bigint | undefined)[] {
		return this.resolved().map((range) => {
			if (!range) return undefined;
			const start = max(range.start, from);
			return range.end === undefined || range.end > start ? start : undefined;
		});
	}

	/** The time starts still looked up, by time. Mirrors the Rust `Window::lookups`. */
	lookups(): Lookup[] {
		const lookups: Lookup[] = [];
		for (const { positions } of this.#looking.all) {
			for (const position of positions) {
				const looking = this.#spans[position].looking;
				if (looking) lookups.push(looking);
			}
		}
		return lookups;
	}

	/**
	 * The lookup of time start `time` answered with `group`, the group containing it or the oldest
	 * group of the history when `time` is older than all of it: its spans start there, once that
	 * group is held. Undefined, or an answer not held, means the history couldn't tell, so they
	 * start where the groups held say. Returns whether the groups in range moved. Mirrors the Rust
	 * `Window::found`.
	 */
	found(at: bigint, group: bigint | undefined, holdings: Holdings = UNHELD): boolean {
		const found = this.#looking.take(at);
		if (found === undefined) return false;
		const held = group !== undefined && holdings.nextStart(group) === group ? group : undefined;
		const containing = holdings.containing(at);
		const start = held ?? (typeof containing === "object" ? containing.group : undefined);
		// A span looking up its start read nothing, so each one found only reads more.
		for (const position of found) {
			this.#unname(position);
			const span = this.#spans[position];
			span.looking = undefined;
			span.start = start ?? 0n;
			span.floored = start !== undefined;
			this.#name(position);
		}
		this.#grow(found);
		return true;
	}

	/**
	 * What rebuilding from the spans would read, may ask a miss for, and name as its start, for a test
	 * to check the runs grown span by span against.
	 */
	fresh(): { runs: Run[]; missable: Run[]; named: bigint | undefined } {
		const groups: Run[] = [];
		const missable: Run[] = [];
		let named: bigint | undefined;
		for (const span of this.#spans) {
			const [reads, asks] = spanReads(span);
			if (reads) groups.push(reads);
			if (asks) missable.push(asks);
			if (span.floored) named = min(named ?? span.start, span.start);
		}
		return { runs: merge(groups), missable: merge(missable), named };
	}

	/** The lowest start the spans name, counted. */
	named(): bigint | undefined {
		return this.#named[0]?.start;
	}

	// Whether a time start, a join's reach back, or a relative start waits on the groups held to
	// place it.
	#unplaced(): boolean {
		return this.#waiting.size > 0 || this.#reaching.length > 0 || this.#counting.length > 0;
	}

	// The first run ending past `key`.
	#runAt(key: bigint): Run | undefined {
		return this.#runs[above(this.#runs, key)];
	}

	#place(latest: bigint | undefined, holdings: Holdings): void {
		this.#spans = [];
		this.#pending = false;
		this.#waiting = new ByTime();
		this.#looking = new ByTime();
		this.#reaching = [];
		this.#counting = [];
		this.#named = [];
		this.#unnamed = 0;
		for (let index = 0; index < this.ranges.length; index++) {
			if (!isEmpty(this.ranges[index])) this.#placeRange(index, latest, holdings);
		}
		// Each span waiting was just tried against these holdings.
		this.#waited = holdings.unplaced?.()?.mark;
		this.#rebuild();
	}

	// Resolve the non-empty range at `index` into a span, or note what it waits on.
	#placeRange(index: number, latest: bigint | undefined, holdings: Holdings): void {
		const range = this.ranges[index];
		const until = endOf(range.end, latest, holdings);
		if (until === undefined) {
			this.#pending = true;
			return;
		}
		let anchor = range.start;
		const min = joins(range, this.min, holdings);
		if (min !== undefined) {
			const reached = latest === undefined ? undefined : holdings.reachBack?.(latest, min);
			if (!reached) {
				// Nothing to measure the live edge by yet: with groups held, read none of them until one
				// measures it; with none, wait as a relative start does.
				if (latest === undefined) this.#pending = true;
				else this.#reaching.push(index);
				return;
			}
			anchor = reached.anchor;
		}
		const start = startOf(anchor, latest, holdings);
		if (start === "pending") {
			this.#pending = true;
			return;
		}
		if (start === "counting") {
			this.#counting.push(index);
			return;
		}
		if (start === "waiting") {
			// Reads only the groups from its time until the groups held place it; see #startWaiting.
			this.#waiting.add(time(anchor) ?? 0n, this.#spans.length);
			this.#push({ index, anchor, start: 0n, end: until, floored: false, waiting: true });
			return;
		}
		// A time start the groups held can't place reads nothing until its lookup answers. A join's
		// reach back never looks one up: it starts from what this hop holds.
		const at = time(anchor);
		const asked = time(range.start) !== undefined && at === time(range.start);
		const below = at === undefined || !asked ? undefined : unresolved(at, start.group, start.floored, holdings);
		const looking = at !== undefined && below !== undefined ? { time: at, below } : undefined;
		if (looking) this.#looking.add(looking.time, this.#spans.length);
		const counted = !start.floored && relative(anchor) !== undefined;
		this.#push({
			index,
			anchor,
			start: start.group,
			end: until,
			floored: start.floored,
			looking,
			waiting: false,
			reach: counted ? (holdings.nextStart(0n) ?? start.group) : undefined,
		});
	}

	#push(span: Span): void {
		this.#spans.push(span);
		this.#name(this.#spans.length - 1);
	}

	// Count where the span at `position` starts in {@link start}.
	#name(position: number): void {
		const span = this.#spans[position];
		if (!span.floored) {
			this.#unnamed++;
			return;
		}
		const named = this.#named;
		const at = lowerBound(named.length, (i) => named[i].start >= span.start);
		if (named[at]?.start === span.start) named[at].count++;
		else named.splice(at, 0, { start: span.start, count: 1 });
	}

	// Stop counting where the span at `position` starts, before it moves.
	#unname(position: number): void {
		const span = this.#spans[position];
		if (!span.floored) {
			this.#unnamed--;
			return;
		}
		const named = this.#named;
		const at = lowerBound(named.length, (i) => named[i].start >= span.start);
		if (named[at]?.start !== span.start) return;
		if (--named[at].count === 0) named.splice(at, 1);
	}

	#updateSettled(): void {
		this.#settled = !this.#pending && !this.#unplaced() && this.#looking.size === 0;
	}

	// The groups in range: every span that knows where it starts, through its end, and every span
	// waiting on a time start from that time.
	#rebuild(): void {
		this.#updateSettled();
		const groups: Run[] = [];
		const missable: Run[] = [];
		for (const span of this.#spans) {
			const [reads, asks] = spanReads(span);
			if (reads) groups.push(reads);
			if (asks) missable.push(asks);
		}
		this.#runs = merge(groups);
		this.#missable = merge(missable);
	}

	// Take in the spans at `positions`, each now reading at least what it did before: their runs
	// are inserted into the sorted runs, so a change to a few spans costs them, not every range.
	#grow(positions: readonly number[]): void {
		this.#updateSettled();
		for (const position of positions) {
			const [reads, asks] = spanReads(this.#spans[position]);
			if (reads) insert(this.#runs, reads);
			if (asks) insert(this.#missable, asks);
		}
	}
}

/** Spans by position, keyed by the time each waits on or looks up, lowest first. */
type Timed = { time: bigint; positions: number[] };

/** {@link Timed} entries kept sorted by time, so the times in a run are found without a walk. */
class ByTime {
	#entries: Timed[] = [];

	get size(): number {
		return this.#entries.length;
	}

	get all(): readonly Timed[] {
		return this.#entries;
	}

	// The index of the first entry at or after `time`.
	#at(time: bigint): number {
		const entries = this.#entries;
		return lowerBound(entries.length, (i) => entries[i].time >= time);
	}

	/** Add the span at `position` under `time`. */
	add(time: bigint, position: number): void {
		const at = this.#at(time);
		const entry = this.#entries[at];
		if (entry?.time === time) entry.positions.push(position);
		else this.#entries.splice(at, 0, { time, positions: [position] });
	}

	/** Put back an entry taken out, whose time no entry holds. */
	put(entry: Timed): void {
		this.#entries.splice(this.#at(entry.time), 0, entry);
	}

	/** Take out the spans under `time`, if any. */
	take(time: bigint): number[] | undefined {
		const at = this.#at(time);
		if (this.#entries[at]?.time !== time) return undefined;
		return this.#entries.splice(at, 1)[0].positions;
	}

	/** Take out every entry whose time is in `[start, end)`, lowest first. */
	takeIn(start: bigint, end: bigint): Timed[] {
		const from = this.#at(start);
		const to = end >= UNBOUNDED ? this.#entries.length : this.#at(end);
		return to > from ? this.#entries.splice(from, to - from) : [];
	}

	/** Take out every entry, lowest first. */
	takeAll(): Timed[] {
		const entries = this.#entries;
		this.#entries = [];
		return entries;
	}
}

// What `span` reads, and the part of it a miss may ask for, each undefined for none.
function spanReads(span: Span): [Run | undefined, Run | undefined] {
	// A span still looking up its start reads nothing until it knows which group that is.
	if (span.looking) return [undefined, undefined];
	// A span waiting on its time start reads from that time meanwhile, and may ask a miss there, as
	// one reaching back past every group held does: whichever group contains it starts at or before
	// it, so every group from there on is in range.
	if (span.waiting) {
		const at = time(span.anchor) ?? 0n;
		if (at >= span.end) return [undefined, undefined];
		return [
			{ start: at, end: span.end },
			{ start: at, end: span.end },
		];
	}
	if (span.start >= span.end) return [undefined, undefined];
	// A time start before every group held reaches back to the group containing it, which is unknown
	// here, so it asks no miss below its time: every key from there on is in range, the key at the
	// time itself being the containing group if it exists. A relative one counting back past every
	// group held asks none below its reach.
	let reach = span.start;
	if (!span.floored && span.anchor !== undefined) {
		reach = time(span.anchor) ?? span.reach ?? span.start;
	}
	return [{ start: span.start, end: span.end }, reach < span.end ? { start: reach, end: span.end } : undefined];
}

// The first index in `[0, length)` for which `past` holds, `past` being false then true.
function lowerBound(length: number, past: (index: number) => boolean): number {
	let lo = 0;
	let hi = length;
	while (lo < hi) {
		const mid = (lo + hi) >>> 1;
		if (past(mid)) hi = mid;
		else lo = mid + 1;
	}
	return lo;
}

// The index of the first of sorted, disjoint `runs` ending past `key`.
function above(runs: readonly Run[], key: bigint): number {
	return lowerBound(runs.length, (i) => runs[i].end > key);
}

// Insert `run` into sorted, disjoint `runs`, merging what it overlaps or touches. An empty one
// changes nothing.
function insert(runs: Run[], run: Run): void {
	if (run.end <= run.start) return;
	const from = lowerBound(runs.length, (i) => runs[i].end >= run.start);
	const to = lowerBound(runs.length, (i) => runs[i].start > run.end);
	if (from === to) {
		runs.splice(from, 0, { ...run });
		return;
	}
	const merged = { start: min(runs[from].start, run.start), end: max(runs[to - 1].end, run.end) };
	runs.splice(from, to - from, merged);
}

// The times outside sorted, disjoint `runs`, as runs lowest first.
function outside(runs: readonly Run[]): Run[] {
	const gaps: Run[] = [];
	let from = 0n;
	for (const run of runs) {
		if (run.end <= run.start) continue;
		if (run.start > from) gaps.push({ start: from, end: run.start });
		from = max(from, run.end);
	}
	if (from < UNBOUNDED) gaps.push({ start: from, end: UNBOUNDED });
	return gaps;
}

// `runs` merged into sorted, disjoint runs, the empty ones dropped.
function merge(runs: Run[]): Run[] {
	const merged: Run[] = [];
	for (const run of runs.filter((run) => run.end > run.start).sort((a, b) => order(a.start, b.start))) {
		const last = merged.at(-1);
		if (last && run.start <= last.end) last.end = max(last.end, run.end);
		else merged.push({ ...run });
	}
	return merged;
}

// The parts of sorted, disjoint `runs` inside `[from, end)`.
function intersect(runs: readonly Run[], from: bigint, end: bigint): Run[] {
	const out: Run[] = [];
	for (const run of runs) {
		if (run.end <= from) continue;
		if (run.start >= end) break;
		out.push({ start: max(run.start, from), end: min(run.end, end) });
	}
	return out;
}

/**
 * The Min Age a live join reaches back by: `min`, for {@link LIVE}, where the hop resolves its own
 * ranges. Any other start names where it starts, the oldest one included, which asks for everything
 * the budget keeps. A zero minimum starts at the latest group anyway. Mirrors the Rust `joins`.
 */
function joins(range: Range, min: Duration | undefined, holdings: Holdings): Duration | undefined {
	const live = relative(range.start) === 0n && range.end === undefined;
	return live && min !== undefined && !min.isZero() && holdings.reachBack && holdings.resolves() ? min : undefined;
}

/**
 * The first group a range starts at, and whether it names that group rather than reaching back to
 * the oldest one. `"pending"` while it waits on a latest group, reading everything meanwhile,
 * `"waiting"` for a time start the groups held can't place yet, and `"counting"` for a relative
 * start they can't count back to yet, reading nothing meanwhile. Mirrors the Rust `start`.
 */
function startOf(
	anchor: Anchor | undefined,
	latest: bigint | undefined,
	holdings: Holdings,
): { group: bigint; floored: boolean } | "waiting" | "pending" | "counting" {
	// Nothing sits below group 0, so the oldest start needs no latest group.
	if (anchor === undefined) return { group: 0n, floored: false };
	if (anchor.relative !== undefined) {
		if (latest === undefined) return "pending";
		const counted = holdings.countedStart?.(latest, anchor.relative);
		if (counted === "unknown") return "counting";
		// Fewer groups are held than it counts back, so reach back to the oldest.
		if (counted === "before") return { group: 0n, floored: false };
		const group = counted?.group ?? holdings.back(latest, anchor.relative);
		return group === undefined ? { group: 0n, floored: false } : { group, floored: true };
	}
	// No group starts before 0, so time 0 names group 0 whatever is held.
	if (anchor.time === 0n) return { group: 0n, floored: true };
	const containing = holdings.containing(anchor.time);
	if (typeof containing === "object") return { group: containing.group, floored: true };
	// The group holding it is older than any held, so reach back to the oldest.
	if (containing === "before") return { group: 0n, floored: false };
	// Before any group, as for a relative anchor, which a hop that holds none (such as a reader
	// following route copies) leaves to the copies to resolve.
	if (latest === undefined && !holdings.resolves()) return "pending";
	// Nothing can arrive any more, so nothing held reaches past it.
	if (holdings.complete()) return { group: 0n, floored: false };
	// The groups held can't place it, so wait for one that can rather than guess from the latest:
	// a copy can see the live edge before the groups its publisher resolved the time to.
	return "waiting";
}

/**
 * The exclusive end of the run a lookup of time start `at`, resolved to `start`, searches, when
 * the groups held can't tell which group contains it and a handler may: it is before every group
 * held, or a group the cache evicted may start after `start`, the newest held at or before it, and
 * still at or before `at`. Starts the producer skipped hold none. Mirrors the Rust `unresolved`.
 */
function unresolved(at: bigint, start: bigint, floored: boolean, holdings: Holdings): bigint | undefined {
	if (!holdings.looksUp?.()) return undefined;
	if (!floored) return holdings.nextStart(0n);
	const next = start + 1n;
	const after = holdings.nextStart(next);
	if (after === undefined) return undefined;
	const gap = min(after, at + 1n);
	return holdings.neverProduced?.(next, gap) || next >= gap ? undefined : after;
}

/**
 * Where a range stops, exclusive ({@link UNBOUNDED} past every group), or undefined while it waits
 * on a latest group. Every end includes its group. Mirrors the Rust `end`.
 */
function endOf(anchor: Anchor | undefined, latest: bigint | undefined, holdings: Holdings): bigint | undefined {
	if (anchor === undefined) return UNBOUNDED;
	if (anchor.time !== undefined) return anchor.time + 1n;
	if (latest === undefined) return undefined;
	const group = holdings.back(latest, anchor.relative);
	// It counts back past every group held, so it ends before them all.
	return group === undefined ? 0n : group + 1n;
}

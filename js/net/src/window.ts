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

import { Duration } from "@moq/clock";
import { Counts, Cover } from "./cover.ts";
import { NO_MAX_DELAY, wireMillis } from "./internal.ts";
import type { Range, Start } from "./track.ts";
import { max, min, UNBOUNDED } from "./util/key.ts";
import { Sorted } from "./util/sorted.ts";

/** A run of group starts `[start, end)`; an open run ends at {@link UNBOUNDED}. */
export type Run = { start: bigint; end: bigint };

/**
 * One range resolved to groups: from `start` up to the exclusive `end`, undefined while open.
 * `floored` says whether the start names a group, rather than reaching back to the oldest one.
 */
export type Resolved = { start: bigint; end?: bigint; floored: boolean };

/** Check one end's shape, naming `field` in the error. */
function timeOf(field: string, value: unknown): bigint | undefined {
	if (value === undefined) return undefined;
	if (typeof value === "bigint" && value >= 0n) return value;
	throw new TypeError(`${field} must be a time, a non-negative bigint`);
}

/** Check one start's shape: a time, or a live join's lag rounded up to a whole millisecond. */
function checkStart(value: unknown): Start | undefined {
	if (typeof value === "object" && value !== null && "live" in value) {
		const live = (value as { live: unknown }).live;
		if (!(live instanceof Duration)) throw new TypeError("a live range start must be a Duration");
		return { live: wireMillis("live range start", live) };
	}
	return timeOf("range start", value);
}

/** Check and copy a caller's ranges, refusing a malformed one rather than serving it widened. */
export function rangesOf(ranges: readonly Range[]): Range[] {
	if (!Array.isArray(ranges)) throw new TypeError("ranges must be an array");
	return ranges.map((range) => {
		if (typeof range !== "object" || range === null) throw new TypeError("a range must be an object");
		const out: Range = {};
		const start = checkStart(range.start);
		const end = timeOf("range end", range.end);
		if (start !== undefined) out.start = start;
		if (end !== undefined) out.end = end;
		return out;
	});
}

/** The time `range` starts from, undefined when it doesn't name one. Mirrors the Rust `Range::time`. */
export function rangeTime(range: Range): bigint | undefined {
	return typeof range.start === "bigint" ? range.start : undefined;
}

/**
 * The lag of the live join `range` starts with, in whole milliseconds rounded up as the wire
 * carries it, undefined for a range that doesn't join live. Mirrors the Rust `live_millis`.
 */
export function liveMillis(range: Range): number | undefined {
	return typeof range.start === "object" ? Math.ceil(range.start.live.asMillis()) : undefined;
}

/**
 * The range from group `start` up to the exclusive group `end`, or onward for undefined: what a
 * wire's group bounds become. Undefined when it holds nothing, which an exclusive end of 0 says
 * whatever the start. Mirrors the Rust `Range::span`.
 */
export function span(start: bigint | undefined, end: bigint | undefined): Range | undefined {
	if (end === 0n) return undefined;
	const range: Range = {};
	if (start !== undefined) range.start = start;
	if (end !== undefined) range.end = end - 1n;
	return isEmpty(range) ? undefined : range;
}

/** Whether `range` certainly delivers nothing: it ends before a time start. */
export function isEmpty(range: Range): boolean {
	const time = rangeTime(range);
	return time !== undefined && range.end !== undefined && range.end < time;
}

/**
 * A canonical order, as the Rust `Range` sorts: from the oldest group first, then by time, then
 * live joins by lag, then by end, open first.
 */
export function compare(a: Range, b: Range): number {
	const rank = (range: Range): [number, bigint] => {
		const live = liveMillis(range);
		if (live !== undefined) return [2, BigInt(live)];
		const time = rangeTime(range);
		return time === undefined ? [0, 0n] : [1, time];
	};
	const [ra, va] = rank(a);
	const [rb, vb] = rank(b);
	return ra - rb || order(va, vb) || order(a.end ?? -1n, b.end ?? -1n);
}

function order(a: bigint, b: bigint): number {
	return a < b ? -1 : a > b ? 1 : 0;
}

/** Whether two lists hold the same ranges in the same order. */
export function sameRanges(a: readonly Range[], b: readonly Range[]): boolean {
	return a.length === b.length && a.every((range, index) => rangeKey(range) === rangeKey(b[index]));
}

/** A range as a string, equal exactly when the ranges are; JSON can't hold a bigint. */
export function rangeKey(range: Range): string {
	const live = liveMillis(range);
	const start = live === undefined ? (rangeTime(range) ?? "") : `L${live}`;
	return `${start}..${range.end ?? ""}`;
}

/**
 * The union of `ranges` in a canonical order, without empty or redundant ranges.
 *
 * Time spans merge where they overlap or touch, as the group starts they cover. The rest can't be
 * placed without the live edge, so only an open range drops what it certainly covers: the open
 * time span drops ranges starting inside it, and a range from the oldest group following the live
 * edge covers everything. A live join reaches no further back than its lag, so it covers nothing
 * else. Mirrors the Rust `union`.
 */
export function union(ranges: readonly Range[]): Range[] {
	const kept = ranges.filter((range) => !isEmpty(range));
	if (kept.some((range) => range.start === undefined && range.end === undefined)) return [{}];

	const spans: Run[] = [];
	for (const range of kept) {
		const time = rangeTime(range);
		if (time === undefined) continue;
		spans.push({ start: time, end: range.end === undefined ? UNBOUNDED : range.end + 1n });
	}
	spans.sort((a, b) => order(a.start, b.start) || order(a.end, b.end));
	const runs: Run[] = [];
	for (const span of spans) {
		const last = runs.at(-1);
		if (last && span.start <= last.end) last.end = max(last.end, span.end);
		else runs.push({ ...span });
	}

	const out: Range[] = runs.map((run) =>
		run.end === UNBOUNDED ? { start: run.start } : { start: run.start, end: run.end - 1n },
	);
	// A range with no time start can't be placed against the time spans.
	for (const range of kept) if (rangeTime(range) === undefined) out.push(range);

	out.sort(compare);
	return out.filter((range, index) => index === 0 || rangeKey(range) !== rangeKey(out[index - 1]));
}

/**
 * The lowest time any range starts from, or undefined when one names none: the floor a wire
 * without ranges carries. Mirrors the Rust `Subscription::start`.
 */
export function rangesStart(ranges: readonly Range[]): bigint | undefined {
	let floor: bigint | undefined;
	for (const range of ranges) {
		if (isEmpty(range)) continue;
		const time = rangeTime(range);
		if (time === undefined) return undefined;
		floor = min(floor ?? time, time);
	}
	return floor;
}

/**
 * The exclusive end past every range, or undefined when one is open: the cap a wire without
 * ranges carries. No range at all ends at group 0. Mirrors the Rust `Subscription::end`.
 */
export function rangesEnd(ranges: readonly Range[]): bigint | undefined {
	let end = 0n;
	for (const range of ranges) {
		if (isEmpty(range)) continue;
		if (range.end === undefined) return undefined;
		end = max(end, range.end + 1n);
	}
	return end;
}

/** The ranges a SUBSCRIBE carries for `ranges`: all but the empty ones. Mirrors the Rust `wire_ranges`. */
export function wireRanges(ranges: readonly Range[]): Range[] {
	return ranges.filter((range) => !isEmpty(range));
}

/** Whether every range has an end, so a lite-07 publisher ends the subscription once it covers them. */
export function bounded(ranges: readonly Range[]): boolean {
	return ranges.every((range) => range.end !== undefined);
}

/**
 * Whether a subscriber joins at the live edge: an open range with no time start, a live join or one
 * from the oldest group, following it. A time start resumes a known group instead, such as a route
 * switch picking up where the reader stopped. Mirrors the Rust `Prefs::joins_live`.
 */
export function joinsLive(ranges: readonly Range[] | undefined): boolean {
	return (ranges ?? [{}]).some((range) => rangeTime(range) === undefined && range.end === undefined);
}

/**
 * The one span a wire without ranges carries for `ranges`, which the subscribers' own reads
 * narrow again: from the lowest time start up to {@link rangesEnd}.
 *
 * A subscription whose ranges all lack a time start asks with no start, which a pre-06 publisher
 * reads as the latest group and a lite-06 one as no floor: a wire without ranges carries a live
 * join so. One mixing the two asks from group 0, since no single start covers both: the latest
 * group may sit below the time start. Mirrors the Rust `wire_span`.
 */
export function wireSpan(ranges: readonly Range[]): { start?: bigint; end?: bigint } {
	let start = rangesStart(ranges);
	if (start === undefined && ranges.some((range) => !isEmpty(range) && rangeTime(range) !== undefined)) start = 0n;
	return { start, end: rangesEnd(ranges) };
}

/** A range holding nothing: nothing sorts below group 0. Mirrors the Rust `Range::EMPTY`. */
export const EMPTY: Range = Object.freeze({ start: 1n, end: 0n });

/**
 * `ranges` with every range raised to start no earlier than `floor` and stop before the exclusive
 * `end`, without ever widening one. Mirrors the Rust `Subscription::clamped`.
 *
 * `resolved` names where each range resolved, by index, so its start becomes its own group,
 * raised to `floor` or capped at `end`. A start not resolved keeps its time, since the floor may
 * lie before it, so the caller drops what it resolves to below `floor`. An end not resolved
 * becomes the cap.
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
		let start = at ? at.start : range.start;
		if (floor !== undefined && typeof start === "bigint") start = max(start, floor);
		let requested = range.end;
		if (requested !== undefined && at?.end !== undefined) {
			if (at.end === 0n) return EMPTY;
			requested = at.end - 1n;
		}
		if (end !== undefined) {
			if (end === 0n) return EMPTY;
			requested = requested === undefined ? end - 1n : min(requested, end - 1n);
		}
		const out: Range = {};
		if (start !== undefined) out.start = start;
		if (requested !== undefined) out.end = requested;
		return out;
	});
}

/** Where a time falls among the groups a hop holds. Mirrors the Rust `Containing`. */
export type Containing =
	/** The newest group starting at or before it. */
	| { group: bigint }
	/** Every group held starts after it. */
	| "before"
	/** No group is held. */
	| "unknown";

/**
 * The groups a serving hop holds, by start, for resolving time starts and live joins. Mirrors the
 * Rust `Holdings`.
 */
export interface Holdings {
	/**
	 * Whether this hop resolves ranges against groups of its own. A hop that holds none, such as a
	 * reader following route copies, leaves its ranges to the copies.
	 */
	resolves(): boolean;
	/**
	 * Whether this hop leaves a live join to the copies it follows, holding it as before any group
	 * until one places it. Absent for one whose ranges were placed already, where a live join left
	 * with no start reached back. Mirrors the Rust `Holdings::defers`.
	 */
	defers?(): boolean;
	/**
	 * Where `time` falls among the groups held, for a reader whose max delay is `budget` in
	 * milliseconds ({@link NO_MAX_DELAY} for none): a received copy proves some placements only for a
	 * reader within the budget its upstream answered under. Mirrors the Rust `Holdings::containing`.
	 */
	containing(time: bigint, budget: number): Containing;
	/** The lowest group held starting at or after `from`. */
	nextStart(from: bigint): bigint | undefined;
	/** Whether nothing more can arrive here, so a time start nothing contains reaches back to the oldest group rather than wait. */
	complete(): boolean;
	/**
	 * Whether a fill handler can look up a time start the groups held can't place in the track's
	 * history, as the Rust `Holdings::looks_up` says. Absent for none.
	 */
	looksUp?(): boolean;
	/**
	 * Where a live join holding a nonzero `min` behind the live edge, ending at `end`, starts for a
	 * reader within `budget` (see {@link containing}). On a hop that {@link answersJoins}, only an
	 * answer recorded after tick `since` counts. Undefined while it can't tell yet: nothing held
	 * measures the live edge, or a relay copy waits on upstream to answer for it. Absent where the
	 * hop leaves its ranges to the copies it follows. Mirrors the Rust `Holdings::join`.
	 */
	join?(min: Duration, end: bigint | undefined, budget: number, since: number): Join | undefined;
	/**
	 * Whether this hop places a live join only where its upstream's answer to that join does, never
	 * from the groups it holds: a lite-07 relay copy, whose wire carries each join and reports where
	 * each starts. Absent for none. Mirrors the Rust `Holdings::answers_joins`.
	 */
	answersJoins?(): boolean;
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
 * Where a live join starts: as a time start at the live edge less its lag (`time`), which
 * resolves, looks up, and waits as any time start does; at a group this hop measured by arrival,
 * or its upstream answered (`group`); or nowhere (`"empty"`), as the upstream answered a bounded
 * join. Mirrors the Rust `Join`.
 */
export type Join = { group: bigint; time?: never } | { time: bigint; group?: never } | "empty";

/**
 * The times a {@link Holdings} can't place: sorted, disjoint runs, apart from the times a placement
 * that the groups and drops held don't decide places. `mark` changes whenever such a placement may
 * have, so a window that sees it unchanged tries only the times outside `runs` again.
 */
export type Unplaced = { runs: readonly Run[]; mark: number };

/** A hop that holds no groups of its own, such as a reader following route copies that resolve its ranges for it. */
export const UNHELD: Holdings = Object.freeze({
	resolves: () => false,
	defers: () => true,
	containing: () => "unknown" as const,
	nextStart: () => undefined,
	complete: () => true,
});

/**
 * A hop that holds no groups of its own but knows every time it is asked starts a group, such as
 * a publisher's reported start: a time start names the group starting there.
 */
export const AT_KEYS: Holdings = Object.freeze({
	...UNHELD,
	defers: () => false,
	containing: (time: bigint) => ({ group: time }),
});

/** One non-empty range, resolved. */
type Span = {
	/** The time it starts from: its range's start, or a live join's; undefined for the oldest group or a join's group. */
	at: bigint | undefined;
	start: bigint;
	/** The exclusive end, {@link UNBOUNDED} past every group. */
	end: bigint;
	/** Whether the start names a group, rather than reaching back to the oldest one. */
	floored: boolean;
	/**
	 * A time start the groups held can't place, whose group a fill asks the track's history for: the
	 * span reads nothing until it answers.
	 */
	looking?: Lookup;
	/** A time start the groups held can't place yet: the span reads only the groups from that time until it can. */
	waiting: boolean;
	/** What it counts in the window's groups and missable groups, so taking it out uncounts exactly that. */
	reads?: Run;
	asks?: Run;
};

/**
 * A time start being looked up: its time, and the oldest key the group containing it may start at,
 * past every group held below it. Mirrors the Rust `Looking`.
 */
export interface Lookup {
	time: bigint;
	floor: bigint;
}

/**
 * What the track's history answered a time start's lookup with: the group containing the time, no
 * group from `floor`, the oldest key it asked from, through the time, or nothing, with no handler
 * left to. Mirrors the Rust `window::Found`.
 */
export type Found = { kind: "group"; group: bigint } | { kind: "dropped"; floor: bigint } | { kind: "unanswered" };

/**
 * A subscriber's ranges resolved to the group starts it reads, each by the key its caller gave
 * it, so an edit resolves only the ranges it adds and leaves every other where it resolved.
 * Mirrors the Rust `Window`.
 *
 * A start resolves when its range is placed, and again only when it is replaced. A live join
 * waits, reading everything, until {@link settle} names a first group. A time start the groups
 * held can't place yet waits too, reading only the groups from its time meanwhile, since whichever
 * group contains it starts at or before it.
 */
export class Window {
	// What these were resolved from, by key, in key order.
	#ranges = new Map<bigint, Range>();
	// Each non-empty range placed, by key.
	#spans = new Map<bigint, Span>();
	// The groups the spans cover, counted so an edit moves only its own runs.
	#groups = new Cover();
	// The part of {@link #groups} a miss may ask for: see {@link missable}.
	#missable = new Cover();
	// The ranges whose live join waits on a first group, by key: everything is in range meanwhile.
	#pending = new Set<bigint>();
	/**
	 * The spans, by key, whose time start waits on a group arriving, by that time: a change to the
	 * groups held tries only those its {@link Holdings.unplaced} leaves out.
	 */
	#waiting = new ByTime();
	/** The {@link Unplaced.mark} every waiting span was last tried under. */
	#waited: number | undefined;
	/**
	 * The spans, by key, still looking up their time start, which read nothing meanwhile, by that
	 * time: kept apart so an answer finds them without visiting every span.
	 */
	#looking = new ByTime();
	/**
	 * The ranges, by key, of a live join that can't tell where it starts while groups are held, such
	 * as a relay copy waiting on upstream to answer for it, so they read nothing yet: a group held is
	 * no reason to start at it.
	 */
	#reaching = new Set<bigint>();
	/**
	 * The join tick each live join's range was added at, by key: a hop that
	 * {@link Holdings.answersJoins} places it only from an answer recorded since.
	 */
	#since = new Map<bigint, number>();
	// How many spans name each start, and how many name none: see {@link start}.
	#named = new Counts<bigint>();
	#unnamed = 0;
	/**
	 * The subscriber's max delay in milliseconds, {@link NO_MAX_DELAY} for none: a received copy
	 * places some time starts only for a reader within the budget its upstream answered under. Only
	 * a range still to place reads it, so one already started stays where it started.
	 */
	get budget(): number {
		return this.#budget;
	}

	set budget(budget: number) {
		// An answer may place a waiting time start under the new budget with no change to the groups
		// held, so each is tried again.
		if (budget !== this.#budget) this.#waited = undefined;
		this.#budget = budget;
	}

	#budget: number;

	/**
	 * Resolve `ranges`, keyed by position, time starts from `holdings` within `budget` (see
	 * {@link budget}), and a live join back by its lag from the live edge, `latest` being the latest
	 * group.
	 */
	constructor(
		ranges: readonly Range[],
		latest: bigint | undefined,
		holdings: Holdings = UNHELD,
		budget = NO_MAX_DELAY,
	) {
		this.#budget = budget;
		for (const [index, range] of ranges.entries()) this.#ranges.set(BigInt(index), range);
		this.#place(latest, holdings);
	}

	/**
	 * {@link Window} for ranges the caller keys itself, in key order, each live join's range added at
	 * join tick `since` of its key (0 by default). Mirrors the Rust `Window::keyed_since`.
	 */
	static keyed(
		ranges: Iterable<readonly [bigint, Range]>,
		latest: bigint | undefined,
		holdings: Holdings = UNHELD,
		budget = NO_MAX_DELAY,
		since: (key: bigint) => number = () => 0,
	): Window {
		const window = new Window([], undefined, holdings, budget);
		for (const [key, range] of ranges) {
			window.#ranges.set(key, range);
			if (liveMillis(range) !== undefined) window.#since.set(key, since(key));
		}
		window.#place(latest, holdings);
		return window;
	}

	/** What these were resolved from, in key order. Costs every range. */
	get ranges(): Range[] {
		return [...this.#ranges.values()];
	}

	/**
	 * Retire the ranges keyed `retire`, then resolve `add`, leaving every other range where it
	 * resolved. A key added again replaces its range in place; any other key added is past every
	 * key held. Each live join's range is added at join tick `since` of its key (0 by default). Costs
	 * the ranges edited, never the rest. Mirrors the Rust `Window::edit_since`.
	 */
	edit(
		retire: Iterable<bigint>,
		add: readonly (readonly [bigint, Range])[],
		latest: bigint | undefined,
		holdings: Holdings = UNHELD,
		since: (key: bigint) => number = () => 0,
	): void {
		const readded = new Set(add.map(([key]) => key));
		for (const key of retire) {
			this.#retire(key);
			// One added again is replaced in place below, keeping its order.
			if (!readded.has(key)) {
				this.#ranges.delete(key);
				this.#since.delete(key);
			}
		}
		for (const [key, range] of add) {
			this.#retire(key);
			this.#ranges.set(key, range);
			if (liveMillis(range) !== undefined) this.#since.set(key, since(key));
			else this.#since.delete(key);
			if (!isEmpty(range)) this.#placeRange(key, latest, holdings);
		}
	}

	/**
	 * Note each live join range keyed in `since` as added at its tick instead (see
	 * `Prefs.stamp`), placing again any still waiting on an answer, which an answer recorded since
	 * that tick may now place. One already placed stays where it started. Returns whether the groups
	 * in range moved.
	 */
	stamp(
		since: Iterable<readonly [bigint, number]>,
		latest: bigint | undefined,
		holdings: Holdings = UNHELD,
	): boolean {
		const placed = this.#spans.size;
		for (const [key, tick] of since) {
			if (!this.#since.has(key)) continue;
			this.#since.set(key, tick);
			if (!this.#reaching.delete(key)) continue;
			this.#placeRange(key, latest, holdings);
		}
		return this.#spans.size > placed;
	}

	// Take out what the range keyed `key` placed, leaving the range itself.
	#retire(key: bigint): void {
		this.#pending.delete(key);
		this.#reaching.delete(key);
		const span = this.#spans.get(key);
		if (span) this.#uncount(key, span);
	}

	/** Whether a live join still waits on a first group. */
	get pending(): boolean {
		return this.#pending.size > 0;
	}

	/**
	 * The keys of the ranges still waiting on a live join or a time start. Mirrors the Rust
	 * `Window::unsettled`.
	 */
	unsettled(): Set<bigint> {
		const keys = new Set([...this.#pending, ...this.#reaching.keys()]);
		for (const entry of this.#waiting.all()) for (const key of entry.keys) keys.add(key);
		for (const entry of this.#looking.all()) for (const key of entry.keys) keys.add(key);
		return keys;
	}

	/** Whether nothing is left to resolve as groups arrive. */
	get settled(): boolean {
		return this.#pending.size === 0 && !this.#unplaced() && this.#looking.size === 0;
	}

	/**
	 * Resolve what the groups now held settle: live joins once `latest` exists or the holdings can
	 * tell where they start, and time starts once a group arrives. Returns whether the groups in
	 * range moved.
	 */
	settle(latest: bigint | undefined, holdings: Holdings = UNHELD): boolean {
		if (this.pending) {
			if (latest === undefined) return false;
			// Every range places again, as when first resolved: the first group arrives once per track.
			this.#place(latest, holdings);
			return !this.pending;
		}
		let moved = false;
		if (this.#reaching.size > 0) {
			const placed = this.#spans.size;
			const reaching = [...this.#reaching];
			this.#reaching.clear();
			for (const key of reaching) this.#placeRange(key, latest, holdings);
			moved = this.#spans.size > placed;
		}
		if (this.#waiting.size > 0 && this.#startWaiting(holdings)) moved = true;
		return moved;
	}

	// Start the spans whose time start waits on a group, once one arrived, or once nothing can
	// arrive any more. Only the times the holdings may now place are tried, so a change placing one
	// of them costs that one, not every span still waiting. Returns whether any started.
	#startWaiting(holdings: Holdings): boolean {
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
		let started = false;
		for (const entry of tried) {
			const containing = holdings.containing(entry.time, this.budget);
			let start: bigint;
			let floored: boolean;
			if (typeof containing === "object") [start, floored] = [containing.group, true];
			// The group holding it is older than any held, or nothing can arrive.
			else if (containing === "before" || complete) [start, floored] = [0n, false];
			else {
				this.#waiting.put(entry);
				continue;
			}
			started = true;
			for (const key of entry.keys) this.#startSpan(key, entry.time, start, floored, holdings);
		}
		return started;
	}

	// Start the span keyed `key`, whose time start `at` waited, at `start`: looked up where the
	// groups held can't place it, as when first resolved, and otherwise reading its groups.
	#startSpan(key: bigint, at: bigint, start: bigint, floored: boolean, holdings: Holdings): void {
		const span = this.#spans.get(key);
		if (!span) return;
		this.#uncount(key, span);
		span.start = start;
		span.floored = floored;
		span.waiting = false;
		const floor = unresolved(at, start, floored, holdings);
		if (floor !== undefined) span.looking = { time: at, floor };
		this.#count(key, span);
	}

	/**
	 * The time starts still waiting on the groups held, lowest first, which wait under
	 * {@link budget}: what a received copy asks its upstream to place. Mirrors the Rust
	 * `Window::asks`.
	 */
	asks(): bigint[] {
		const times: bigint[] = [];
		for (const entry of this.#waiting.all()) times.push(entry.time);
		return times;
	}

	/**
	 * The live joins still waiting on an answer, as `[lag, end, since]`: the lag in milliseconds, the
	 * range's end, and the join tick its range was added at. What a received copy asks its upstream
	 * to place. Mirrors the Rust `Window::joins`.
	 */
	joins(): [number, bigint | undefined, number][] {
		const joins: [number, bigint | undefined, number][] = [];
		for (const key of this.#reaching) {
			const range = this.#ranges.get(key);
			const lag = range && liveMillis(range);
			if (lag !== undefined) joins.push([lag, range?.end, this.#since.get(key) ?? 0]);
		}
		return joins;
	}

	/**
	 * The times in `[start, end)` of the time starts still waiting on the groups held. The group
	 * containing each starts at or before its time, so one at or past a read's floor and below a
	 * group may still land between them. Mirrors the Rust `Window::waiting_in`.
	 */
	waitingIn(start: bigint, end: bigint): bigint[] {
		const times: bigint[] = [];
		for (const entry of this.#waiting.in(start, end)) times.push(entry.time);
		return times;
	}

	/** Whether group `key` is in range. Everything is while a live join waits on a first group. */
	contains(key: bigint): boolean {
		return this.pending || this.#groups.contains(key);
	}

	/**
	 * Whether a range placed so far holds group `key`: unlike {@link contains}, not every group while a
	 * live join waits on a first group. Mirrors the Rust `Window::holds`.
	 */
	holds(key: bigint): boolean {
		return this.#groups.contains(key);
	}

	/** The lowest group in range at or above `from`, undefined when none is. `from` itself while a live join waits on a first group. */
	next(from: bigint): bigint | undefined {
		return this.pending ? from : this.#groups.next(from);
	}

	/** The lowest group in range, undefined while a start waits. Nothing in range floors at group 0. */
	floor(): bigint | undefined {
		if (this.pending || this.#unplaced()) return undefined;
		return this.#groups.lowest() ?? 0n;
	}

	/**
	 * The lowest group the ranges name, undefined while one reaches back to the oldest group or a
	 * start waits.
	 */
	start(): bigint | undefined {
		if (this.pending || this.#reaching.size > 0 || this.#unnamed > 0) return undefined;
		return this.#named.first();
	}

	/** The exclusive end past every group in range, undefined when one stays open or a start waits. Nothing in range ends at 0. */
	end(): bigint | undefined {
		if (!this.settled) return undefined;
		const end = this.#groups.end();
		return end === UNBOUNDED ? undefined : end;
	}

	/** The runs in range from `from` up to the exclusive `end`, lowest first. */
	runs(from = 0n, end = UNBOUNDED): Run[] {
		if (from >= end) return [];
		if (this.pending) return [{ start: from, end }];
		return this.#groups.runs(from, end);
	}

	/**
	 * The part of {@link runs} a miss may ask a fill for: all of it but what starts before a time
	 * start that fell before every group held. Mirrors the Rust `Window::missable`.
	 */
	missable(from = 0n, end = UNBOUNDED): Run[] {
		if (from >= end) return [];
		if (this.pending) return [{ start: from, end }];
		return this.#missable.runs(from, end);
	}

	/**
	 * The range keyed `key` resolved: undefined for one whose start still waits, or is still looked
	 * up, or that isn't held. One empty as given ends at its start. Mirrors the Rust
	 * `Window::resolved_key`.
	 */
	resolvedKey(key: bigint): Resolved | undefined {
		const range = this.#ranges.get(key);
		if (range === undefined) return undefined;
		const time = rangeTime(range);
		if (time !== undefined && isEmpty(range)) return { start: time, end: time, floored: true };
		const span = this.#spans.get(key);
		if (!span || span.looking || span.waiting) return undefined;
		return { start: span.start, end: span.end === UNBOUNDED ? undefined : span.end, floored: span.floored };
	}

	/** Each range resolved, in key order, as {@link resolvedKey} reads it. Mirrors the Rust `Window::resolved`. */
	resolved(): (Resolved | undefined)[] {
		return [...this.#ranges.keys()].map((key) => this.resolvedKey(key));
	}

	/**
	 * Where each range starts once delivery starts at `from`, in key order: undefined for one with
	 * no group there, or whose start still waits. Mirrors the Rust `Window::starts`.
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
		for (const { keys } of this.#looking.all()) {
			for (const key of keys) {
				const looking = this.#spans.get(key)?.looking;
				if (looking) lookups.push(looking);
			}
		}
		return lookups;
	}

	/**
	 * The lookup of time start `at` answered: its spans start where the answer says, never where the
	 * groups held happen to. Left unanswered because no handler is left, they start where the groups
	 * held say, as with no handler at all, unless the cache since evicted the group their floor was
	 * past and a handler is back: then they look it up again from lower down. Returns whether the
	 * groups in range moved. Mirrors the Rust `Window::found`.
	 */
	found(at: bigint, found: Found, holdings: Holdings = UNHELD): boolean {
		const keys = this.#looking.take(at);
		if (keys === undefined) return false;
		for (const key of keys) {
			const span = this.#spans.get(key);
			if (!span) continue;
			this.#uncount(key, span);
			const asked = span.looking?.floor;
			span.looking = undefined;
			if (found.kind === "group") {
				span.start = found.group;
				span.floored = true;
			} else if (found.kind === "dropped") {
				// A floor is one past the group held below the time, which then holds it; from 0, no
				// group starts at or before it, so the span reads from the time on.
				span.start = found.floor > 0n ? found.floor - 1n : at;
				span.floored = true;
			} else {
				const containing = holdings.containing(at, this.budget);
				span.start = typeof containing === "object" ? containing.group : 0n;
				span.floored = typeof containing === "object";
				if (asked !== undefined) {
					// Nothing held at or before the time any more, maybe nothing at all: the group containing
					// it may start anywhere below the floor asked.
					const floor = span.floored
						? unresolved(at, span.start, true, holdings)
						: holdings.looksUp?.()
							? 0n
							: undefined;
					if (floor !== undefined && floor < asked) span.looking = { time: at, floor };
				}
			}
			this.#count(key, span);
		}
		return true;
	}

	/**
	 * What rebuilding from the spans would read, may ask a miss for, and name as its start, for a test
	 * to check the counted runs against.
	 */
	fresh(): { runs: Run[]; missable: Run[]; named: bigint | undefined } {
		const groups: Run[] = [];
		const missable: Run[] = [];
		let named: bigint | undefined;
		for (const span of this.#spans.values()) {
			const [reads, asks] = spanReads(span);
			if (reads) groups.push(reads);
			if (asks) missable.push(asks);
			if (span.floored) named = min(named ?? span.start, span.start);
		}
		return { runs: merge(groups), missable: merge(missable), named };
	}

	/** The lowest start the spans name, counted. */
	named(): bigint | undefined {
		return this.#named.first();
	}

	// Whether a time start or a live join waits on the groups held to place it.
	#unplaced(): boolean {
		return this.#waiting.size > 0 || this.#reaching.size > 0;
	}

	// Resolve every range again from scratch.
	#place(latest: bigint | undefined, holdings: Holdings): void {
		this.#reaching.clear();
		this.#spans.clear();
		this.#pending.clear();
		this.#waiting = new ByTime();
		this.#looking = new ByTime();
		this.#groups = new Cover();
		this.#missable = new Cover();
		this.#named = new Counts();
		this.#unnamed = 0;
		for (const [key, range] of this.#ranges) {
			if (!isEmpty(range)) this.#placeRange(key, latest, holdings);
		}
		// Each span waiting was just tried against these holdings.
		this.#waited = holdings.unplaced?.()?.mark;
	}

	// Resolve the non-empty range keyed `key` into a span, or note what it waits on.
	#placeRange(key: bigint, latest: bigint | undefined, holdings: Holdings): void {
		const range = this.#ranges.get(key);
		if (range === undefined) return;
		const end = range.end === undefined ? UNBOUNDED : range.end + 1n;
		let at = rangeTime(range);
		const live = typeof range.start === "object" ? range.start.live : undefined;
		// A hop whose copies place the join waits on them, as one with no group yet does.
		if (live !== undefined && holdings.defers?.()) {
			this.#pending.add(key);
			return;
		}
		// Placed already, where a copy resolved it: one left with no start reached back.
		if (live !== undefined && holdings.resolves()) {
			let join: Join | undefined;
			if (holdings.answersJoins?.()) {
				// Only its upstream's answer places it, whatever this hop holds: read none of the groups
				// held until it does.
				join = holdings.join?.(live, range.end, this.budget, this.#since.get(key) ?? 0);
				// Nothing to deliver: a start past its end holds nothing.
				if (join === "empty" && range.end !== undefined) join = { group: range.end + 1n };
				if (join === undefined || join === "empty") {
					this.#reaching.add(key);
					return;
				}
			} else if (latest === undefined) {
				// Nothing to measure the live edge by yet: wait for the first group, reading everything
				// meanwhile.
				this.#pending.add(key);
				return;
			} else if (live.asNanos() === 0) {
				// The latest group holds the live edge. One made before any group then starts at the first,
				// which was the latest when it was placed.
				join = { group: latest };
			} else {
				join = holdings.join?.(live, range.end, this.budget, 0);
				// Nothing to measure the live edge by yet: read none of the groups held until it resolves.
				if (join === undefined || join === "empty") {
					this.#reaching.add(key);
					return;
				}
			}
			// A group a join measured starts there; a time is placed among the groups held.
			if (join.group !== undefined) {
				this.#count(key, { at: undefined, start: join.group, end, floored: true, waiting: false });
				return;
			}
			at = join.time;
		}
		const start = startOf(at, latest, this.budget, holdings);
		if (start === "pending") {
			this.#pending.add(key);
			return;
		}
		if (start === "waiting") {
			// Reads only the groups from its time until the groups held place it; see #startWaiting.
			this.#count(key, { at, start: 0n, end, floored: false, waiting: true });
			return;
		}
		// A time start the groups held can't place reads nothing until its lookup answers.
		const floor = at === undefined ? undefined : unresolved(at, start.group, start.floored, holdings);
		const looking = at !== undefined && floor !== undefined ? { time: at, floor } : undefined;
		this.#count(key, { at, start: start.group, end, floored: start.floored, looking, waiting: false });
	}

	// Hold `span` under `key`, counting the groups it reads and where it starts.
	#count(key: bigint, span: Span): void {
		[span.reads, span.asks] = spanReads(span);
		if (span.reads) this.#groups.add(span.reads.start, span.reads.end, 1);
		if (span.asks) this.#missable.add(span.asks.start, span.asks.end, 1);
		if (span.floored) this.#named.add(span.start);
		else this.#unnamed++;
		if (span.looking) this.#looking.add(span.looking.time, key);
		if (span.waiting) this.#waiting.add(span.at ?? 0n, key);
		this.#spans.set(key, span);
	}

	// Take back what {@link #count} counted for `span`, and take it out of the spans.
	#uncount(key: bigint, span: Span): void {
		if (span.reads) this.#groups.add(span.reads.start, span.reads.end, -1);
		if (span.asks) this.#missable.add(span.asks.start, span.asks.end, -1);
		if (span.floored) this.#named.remove(span.start);
		else this.#unnamed--;
		if (span.looking) this.#looking.remove(span.looking.time, key);
		if (span.waiting) this.#waiting.remove(span.at ?? 0n, key);
		this.#spans.delete(key);
	}
}

/** Spans by key, keyed by the time each waits on or looks up, lowest first. */
type Timed = { time: bigint; keys: Set<bigint> };

const compareTimes = (a: bigint, b: bigint): number => (a < b ? -1 : a > b ? 1 : 0);

/**
 * {@link Timed} entries ordered by time, so the times in a run are found without a walk, and a
 * span joins or leaves its time without shifting every other.
 */
class ByTime {
	#entries = new Sorted<bigint, Timed>((entry) => entry.time, compareTimes);

	get size(): number {
		return this.#entries.size;
	}

	/** Every entry, lowest first. */
	*all(): Generator<Timed> {
		const first = this.#entries.first();
		if (first) yield* this.#entries.from(first.time);
	}

	/** Every entry whose time is in `[start, end)`, lowest first. */
	*in(start: bigint, end: bigint): Generator<Timed> {
		for (const entry of this.#entries.from(start)) {
			if (entry.time >= end) return;
			yield entry;
		}
	}

	/** Add the span keyed `key` under `time`. */
	add(time: bigint, key: bigint): void {
		const entry = this.#entries.ceil(time);
		if (entry?.time === time) entry.keys.add(key);
		else this.#entries.insert({ time, keys: new Set([key]) });
	}

	/** Take the span keyed `key` out from under `time`, if it is there. */
	remove(time: bigint, key: bigint): void {
		const entry = this.#entries.ceil(time);
		if (entry?.time !== time || !entry.keys.delete(key)) return;
		if (entry.keys.size === 0) this.#entries.delete(time);
	}

	/** Put back an entry taken out, whose time no entry holds. */
	put(entry: Timed): void {
		this.#entries.insert(entry);
	}

	/** Take out the spans under `time`, if any. */
	take(time: bigint): Set<bigint> | undefined {
		const entry = this.#entries.ceil(time);
		if (entry?.time !== time) return undefined;
		this.#entries.delete(time);
		return entry.keys;
	}

	/** Take out every entry whose time is in `[start, end)`, lowest first. */
	takeIn(start: bigint, end: bigint): Timed[] {
		const taken: Timed[] = [];
		for (const entry of this.#entries.from(start)) {
			if (end < UNBOUNDED && entry.time >= end) break;
			taken.push(entry);
		}
		for (const entry of taken) this.#entries.delete(entry.time);
		return taken;
	}

	/** Take out every entry, lowest first. */
	takeAll(): Timed[] {
		const taken = [...this.all()];
		this.#entries = new Sorted<bigint, Timed>((entry) => entry.time, compareTimes);
		return taken;
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
		const at = span.at ?? 0n;
		if (at >= span.end) return [undefined, undefined];
		return [
			{ start: at, end: span.end },
			{ start: at, end: span.end },
		];
	}
	if (span.start >= span.end) return [undefined, undefined];
	// A time start before every group held reaches back to the group containing it, which is unknown
	// here, so it asks no miss below its time: every key from there on is in range, the key at the
	// time itself being the containing group if it exists.
	const reach = !span.floored && span.at !== undefined ? span.at : span.start;
	return [{ start: span.start, end: span.end }, reach < span.end ? { start: reach, end: span.end } : undefined];
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

/**
 * The first group a range starting from time `at` starts at (undefined for the oldest group), and
 * whether it names that group rather than reaching back to the oldest one. `"pending"` while a hop
 * that holds no groups waits on a latest group, reading everything meanwhile, and `"waiting"` for
 * a time start the groups held can't place yet. Mirrors the Rust `start`.
 */
function startOf(
	at: bigint | undefined,
	latest: bigint | undefined,
	budget: number,
	holdings: Holdings,
): { group: bigint; floored: boolean } | "waiting" | "pending" {
	// Nothing sits below group 0, so the oldest start needs no latest group.
	if (at === undefined) return { group: 0n, floored: false };
	// No group starts before 0, so time 0 names group 0 whatever is held.
	if (at === 0n) return { group: 0n, floored: true };
	const containing = holdings.containing(at, budget);
	if (typeof containing === "object") return { group: containing.group, floored: true };
	// The group holding it is older than any held, so reach back to the oldest.
	if (containing === "before") return { group: 0n, floored: false };
	// Before any group, which a hop that holds none (such as a reader following route copies)
	// leaves to the copies to resolve.
	if (latest === undefined && !holdings.resolves()) return "pending";
	// Nothing can arrive any more, so nothing held reaches past it.
	if (holdings.complete()) return { group: 0n, floored: false };
	// The groups held can't place it, so wait for one that can rather than guess from the latest:
	// a copy can see the live edge before the groups its publisher resolved the time to.
	return "waiting";
}

/**
 * The oldest key a fill of the group containing time start `at`, resolved to `start`, asks from,
 * when the groups held can't tell which group contains it and a handler may: it is before every
 * group held, or a group the cache evicted may start after `start`, the newest held at or before
 * it, and still at or before `at`. Starts the producer skipped hold none. Mirrors the Rust
 * `unresolved`.
 */
function unresolved(at: bigint, start: bigint, floored: boolean, holdings: Holdings): bigint | undefined {
	if (!holdings.looksUp?.()) return undefined;
	if (!floored) return holdings.nextStart(0n) === undefined ? undefined : 0n;
	const next = start + 1n;
	const after = holdings.nextStart(next);
	if (after === undefined) return undefined;
	const gap = min(after, at + 1n);
	return holdings.neverProduced?.(next, gap) || next >= gap ? undefined : next;
}

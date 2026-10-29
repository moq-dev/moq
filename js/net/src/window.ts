/**
 * A subscriber's own ranges, resolved to the group sequences it reads, and the helpers that
 * translate ranges between the model and the wire. Mirrors the Rust `subscription` module.
 *
 * The aggregate a publisher sees is the union of every subscriber's ranges, so it says what
 * arrives, not what one subscriber asked for. A {@link Window} is the per-subscriber side:
 * resolved at the hop serving the subscriber, it filters that subscriber's reads.
 *
 * @module
 */

import { Timestamp } from "./time.ts";
import type { Anchor, Range } from "./track.ts";

/** A run of group sequences `[start, end)`; an open run ends at infinity. */
export type Run = { start: number; end: number };

/** One range resolved to groups: from `start` up to the exclusive `end`, undefined while open. */
export type Resolved = { start: number; end?: number };

/** Everything from the latest group onward. */
export const LIVE: Range = Object.freeze({ start: Object.freeze({ relative: 0 }) });

/** Check one anchor's shape, naming `field` in the error. */
function anchorOf(field: string, anchor: Anchor): Anchor {
	const keys = typeof anchor === "object" && anchor !== null ? Object.keys(anchor) : [];
	const value = keys.length === 1 ? (anchor as Record<string, unknown>)[keys[0]] : undefined;
	if (keys[0] === "time" && value instanceof Timestamp) return { time: value };
	if ((keys[0] !== "absolute" && keys[0] !== "relative") || !Number.isSafeInteger(value) || (value as number) < 0) {
		throw new TypeError(
			`${field} must be {absolute} or {relative}, a non-negative safe integer, or {time}, a Timestamp`,
		);
	}
	return keys[0] === "absolute" ? { absolute: value as number } : { relative: value as number };
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

/** The exclusive end of an absolute anchor, or undefined for any other end. */
function absolute(anchor: Anchor | undefined): number | undefined {
	return anchor?.absolute;
}

/** How many groups back a relative anchor counts, or undefined for any other anchor. */
function relative(anchor: Anchor | undefined): number | undefined {
	return anchor?.relative;
}

/** The presentation time a time anchor names, or undefined for any other anchor. */
function time(anchor: Anchor | undefined): Timestamp | undefined {
	return anchor?.time;
}

/** Whether `time` presents strictly after `other`, as instants, whatever their scales. */
export function later(time: Timestamp, other: Timestamp): boolean {
	return other.isBefore(time);
}

/** Whether any range starts or ends at a presentation time, which needs a timed track. */
export function hasTime(ranges: readonly Range[]): boolean {
	return ranges.some((range) => time(range.start) !== undefined || time(range.end) !== undefined);
}

/** Whether `range` certainly delivers nothing, wherever the latest group lands. */
export function isEmpty(range: Range): boolean {
	const end = absolute(range.end);
	if (end === undefined) return false;
	// Nothing sorts below group 0, whatever the start.
	if (end === 0) return true;
	// Nothing sorts below group 0, so the oldest start floors there.
	const start = range.start === undefined ? 0 : absolute(range.start);
	return start !== undefined && end <= start;
}

/** An anchor as a sort key: absolute, then relative, then time, with `none` for an omitted one. */
function anchorKey(anchor: Anchor | undefined, none: [number, number]): [number, number] {
	if (anchor === undefined) return none;
	if (anchor.absolute !== undefined) return [0, anchor.absolute];
	if (anchor.relative !== undefined) return [1, anchor.relative];
	return [2, anchor.time.value / anchor.time.scale];
}

/**
 * A canonical order: by start, absolute ones first, then relative ones, the oldest (which counts
 * back the farthest), and times; then by end, open last.
 */
function compare(a: Range, b: Range): number {
	const [x, y] = [
		anchorKey(a.start, [1, Number.POSITIVE_INFINITY]),
		anchorKey(b.start, [1, Number.POSITIVE_INFINITY]),
	];
	const [p, q] = [anchorKey(a.end, [3, 0]), anchorKey(b.end, [3, 0])];
	return x[0] - y[0] || x[1] - y[1] || p[0] - q[0] || p[1] - q[1];
}

/**
 * The union of `ranges` in a canonical order, without empty or redundant ranges.
 *
 * Absolute spans merge where they overlap or touch. The rest can't be placed without the
 * latest group, so only an open range drops what it certainly covers: the open absolute span
 * drops ranges starting inside it, and the farthest open relative range drops relative ranges
 * starting no farther back. The oldest start covers everything, and a time can't be placed
 * against the others without the hop's groups. Mirrors the Rust `union`.
 */
export function union(ranges: readonly Range[]): Range[] {
	const kept = ranges.filter((range) => !isEmpty(range));
	if (kept.some((range) => range.start === undefined && range.end === undefined)) return [{}];

	const spans: Run[] = [];
	for (const range of kept) {
		const start = absolute(range.start);
		if (start === undefined) continue;
		if (range.end === undefined) spans.push({ start, end: Number.POSITIVE_INFINITY });
		else {
			const end = absolute(range.end);
			if (end !== undefined) spans.push({ start, end });
		}
	}
	spans.sort((a, b) => a.start - b.start || a.end - b.end);
	const runs: Run[] = [];
	for (const span of spans) {
		const last = runs.at(-1);
		if (last && span.start <= last.end) last.end = Math.max(last.end, span.end);
		else runs.push({ ...span });
	}
	const tail = runs.at(-1)?.end === Number.POSITIVE_INFINITY ? runs.at(-1)?.start : undefined;
	let farthest: number | undefined;
	for (const range of kept) {
		const back = relative(range.start);
		if (back !== undefined && range.end === undefined) farthest = Math.max(farthest ?? back, back);
	}

	const out: Range[] = runs.map((run) =>
		run.end === Number.POSITIVE_INFINITY
			? { start: { absolute: run.start } }
			: { start: { absolute: run.start }, end: { absolute: run.end } },
	);
	for (const range of kept) {
		const start = absolute(range.start);
		const back = relative(range.start);
		if (start !== undefined) {
			// Merged above unless it ends relative to the latest group.
			if (range.end === undefined || absolute(range.end) !== undefined) continue;
			if (tail === undefined || start < tail) out.push(range);
		} else if (back !== undefined) {
			if (range.end === undefined) continue;
			if (farthest === undefined || back > farthest) out.push(range);
		} else out.push(range);
	}
	if (farthest !== undefined) out.push({ start: { relative: farthest } });

	out.sort(compare);
	return out.filter((range, index) => index === 0 || JSON.stringify(range) !== JSON.stringify(out[index - 1]));
}

/**
 * The lowest absolute group any range asks for, or undefined when one counts back from the
 * latest group or reaches back to the oldest one: the floor a wire without ranges carries.
 */
export function rangesStart(ranges: readonly Range[]): number | undefined {
	let floor: number | undefined;
	for (const range of ranges) {
		const start = absolute(range.start);
		if (start === undefined) return undefined;
		floor = Math.min(floor ?? start, start);
	}
	return floor;
}

/**
 * The exclusive end past every range, or undefined when one is open or ends relative to the
 * latest group: the cap a wire without ranges carries. No range at all ends at group 0.
 */
export function rangesEnd(ranges: readonly Range[]): number | undefined {
	let end = 0;
	for (const range of ranges) {
		const next = absolute(range.end);
		if (next === undefined) return undefined;
		end = Math.max(end, next);
	}
	return end;
}

/**
 * Whether a wire without ranges carries `range` upstream: absolute anchors, and the live and
 * oldest starts it spells with no floor. Mirrors the Rust `Range::is_wire`.
 */
export function isWire(range: Range): boolean {
	const back = relative(range.start);
	return (
		(back === undefined || back === 0) &&
		time(range.start) === undefined &&
		relative(range.end) === undefined &&
		time(range.end) === undefined
	);
}

/**
 * The ranges a SUBSCRIBE carries for `ranges`, without the empty ones. Mirrors the Rust
 * `wire_ranges`.
 *
 * The copy resolves a relative or time end itself, against the groups it receives past it, so
 * it asks through the live edge: SUBSCRIBE_START names a resolved start, but nothing names a
 * resolved end, so one resolved upstream would leave the copy short.
 */
export function wireRanges(ranges: readonly Range[]): Range[] {
	return ranges
		.filter((range) => !isEmpty(range))
		.map((range) =>
			range.end === undefined || absolute(range.end) !== undefined
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
 * such as a route switch picking up where the reader stopped.
 */
export function joinsLive(ranges: readonly Range[] | undefined): boolean {
	return (ranges ?? [{}]).some(
		(range) => absolute(range.start) === undefined && time(range.start) === undefined && range.end === undefined,
	);
}

/**
 * The one span a wire without ranges carries for `ranges`, which the subscribers' own reads
 * narrow again: from the lowest absolute start up to {@link rangesEnd}.
 *
 * A subscription counting only back from the latest group asks with no start, which a pre-06
 * publisher reads as the latest group and a lite-06 one as no floor. One mixing the two asks
 * from group 0, since no single start covers both: the latest group may sit below the absolute
 * start. Mirrors the Rust `wire_span`.
 */
export function wireSpan(ranges: readonly Range[]): { start?: number; end?: number } {
	let start = rangesStart(ranges);
	if (start === undefined && ranges.some((range) => absolute(range.start) !== undefined)) start = 0;
	return { start, end: rangesEnd(ranges) };
}

/**
 * `ranges` with every range raised to start no earlier than `floor` and stop no later than the
 * exclusive `end`, without ever widening one. Mirrors the Rust `Subscription::clamped`.
 *
 * `resolved` names where each range resolved, by index, so a relative or time anchor becomes its
 * own group before it is clamped. A start not resolved keeps its anchor, since the floor may lie
 * before it, so the caller drops what it resolves to below `floor`. The oldest start never lies
 * above the floor, so it becomes the floor. An end not resolved becomes the cap, except a time
 * end, which stays so the caller enforces `end` apart from it.
 */
export function clamped(
	ranges: readonly Range[],
	floor: number | undefined,
	end: number | undefined,
	resolved: readonly (Resolved | undefined)[],
): Range[] {
	if (floor === undefined && end === undefined) return [...ranges];
	return ranges.map((range, index) => {
		const at = resolved[index];
		let start = range.start;
		if (at && absolute(start) === undefined && start !== undefined) start = { absolute: at.start };
		let requested = range.end;
		if (at?.end !== undefined && requested !== undefined && absolute(requested) === undefined)
			requested = { absolute: at.end };
		const named = absolute(start);
		if (floor !== undefined && named !== undefined) start = { absolute: Math.max(named, floor) };
		else if (floor !== undefined && start === undefined) start = { absolute: floor };
		if (end !== undefined && time(requested) === undefined) {
			const capped = absolute(requested);
			requested = { absolute: capped === undefined ? end : Math.min(capped, end) };
		}
		const out: Range = {};
		if (start !== undefined) out.start = start;
		if (requested !== undefined) out.end = requested;
		return out;
	});
}

/** Where a presentation time falls among the groups a hop holds. Mirrors the Rust `Containing`. */
export type Containing =
	/** The newest group starting at or before it. */
	| { group: number }
	/** Every group with a known start begins after it. */
	| "before"
	/** No group has a known start. */
	| "unknown";

/** What a held group says about where it starts. Mirrors the Rust `Held`. */
export type Held =
	/** Its frame 0 presented at this time. */
	| { started: Timestamp }
	/** It holds frame 0 and has not presented it yet. */
	| "pending"
	/** It never will: it ended empty or failed. */
	| "never";

/**
 * When a serving hop's groups start, for resolving time anchors. Mirrors the Rust `Holdings`.
 *
 * Each query answers from ordered indexes, so a time end that walks them forward a step at a
 * time pays for each group or run it passes once, however long it stays unsettled.
 */
export interface Holdings {
	/** Where `pts` falls among the groups with a known start. */
	containing(pts: Timestamp): Containing;
	/** The lowest group at or above `from` known to start, with where it starts. */
	nextStart(from: number): { sequence: number; start: Timestamp } | undefined;
	/** The lowest held group at or above `from`, with what it says about its start. */
	nextHeld(from: number): { sequence: number; held: Held } | undefined;
	/**
	 * The exclusive end, past `sequence`, of the run of sequences from it that can no longer
	 * arrive (dropped, given up, or below where the feed starts), undefined while it may.
	 */
	gone(sequence: number): number | undefined;
	/** Whether nothing more can arrive or start here, so a time end has nothing to wait on. */
	complete(): boolean;
}

/**
 * A hop that holds no groups of its own, such as a reader following route copies that resolve
 * its ranges for it: a time end there filters nothing.
 */
export const UNHELD: Holdings = Object.freeze({
	containing: () => "unknown" as const,
	nextStart: () => undefined,
	nextHeld: () => undefined,
	gone: () => undefined,
	complete: () => true,
});

/**
 * A time end not settled yet, and how far the walk to settle it got. Mirrors the Rust `TimeEnd`.
 *
 * Timestamps never rewind, so the end falls before the lowest group starting past `pts`, once
 * every sequence between it and the newest one starting at or before `pts` is accounted for.
 * Every position below only moves forward, so each group or run of sequences is passed once,
 * whatever a peer leaves missing.
 */
class TimeEnd {
	readonly pts: Timestamp;
	/** The newest group known to start at or before `pts`: it and everything below it in the span is in range. */
	known?: number;
	/** The lowest group known to start after `pts`: where the end falls once settled. */
	after?: number;
	/** Where the walk over held groups resumes: those below it started, or never will. */
	held: number;
	/** Where settling resumes: every sequence from above `known` up to it is accounted for. */
	accounted: number;
	/** The exclusive cap on what is read meanwhile, undefined for none. */
	limit?: number;
	/** The group the cap admits on speculation: the first above `known` that may still start, which may start past `pts`. */
	speculative?: number;

	constructor(pts: Timestamp, start: number) {
		this.pts = pts;
		this.held = start;
		this.accounted = start;
		this.limit = start;
	}

	/** The lowest sequence not known to be in range. */
	unknown(start: number): number {
		return this.known === undefined ? start : Math.max(this.known + 1, start);
	}

	/** Walk the held groups on from where the last call stopped. Returns where the end falls once it settles. */
	advance(start: number, holdings: Holdings): number | undefined {
		// The starts index passes a group that may still start, so a stalled one never keeps a
		// later group known to start inside the end from being read.
		for (;;) {
			const next = holdings.nextStart(this.unknown(start));
			if (!next || (this.after !== undefined && next.sequence >= this.after)) break;
			if (later(next.start, this.pts)) this.after = next.sequence;
			else this.known = next.sequence;
		}

		// The first sequence above `known` that may still start is the one read on speculation:
		// a held group yet to start, or a missing one that may arrive. Only a group starting
		// inside the end is jumped to, since it puts all below it in range.
		this.speculative = undefined;
		for (;;) {
			const at = Math.max(this.held, this.unknown(start));
			if (this.after !== undefined && at >= this.after) break;
			let next = holdings.nextHeld(at);
			if (next && this.after !== undefined && next.sequence >= this.after) next = undefined;
			const held = next?.held;
			if (next && next.sequence === at) {
				if (held === "pending") {
					this.held = at;
					this.speculative = at;
					break;
				}
				if (typeof held === "object") {
					if (later(held.started, this.pts)) this.after = at;
					else this.known = at;
				}
			} else if (next && typeof held === "object" && !later(held.started, this.pts)) {
				this.known = next.sequence;
			} else if (!next && this.after === undefined) {
				// Past every held group: the next to arrive is the speculative one, judged once a
				// settle sees it, before it is read.
				this.held = at;
				break;
			} else {
				const end = holdings.gone(at);
				if (end !== undefined && end > at) {
					this.held = end;
					continue;
				}
				this.held = at;
				this.speculative = holdings.complete() ? undefined : at;
				break;
			}
			this.held = at + 1;
		}
		if (holdings.complete()) this.limit = undefined;
		else if (this.speculative !== undefined) this.limit = this.speculative + 1;
		// Every held group is accounted for: the next to arrive is the speculative one.
		else this.limit = this.after;

		// Settle once every sequence up to the first group starting past `pts` can no longer
		// start inside it. A sequence that arrived only as a datagram never indexes where its
		// group starts, so it counts as gone here.
		const after = this.after;
		if (after === undefined) return undefined;
		// Everything at or below `known` is in range, and `after` lies above it.
		this.accounted = Math.max(this.accounted, this.unknown(start));
		while (this.accounted < after) {
			const next = holdings.nextHeld(this.accounted);
			if (next && next.sequence === this.accounted) {
				if (next.held === "pending") return undefined;
				// It started past `pts` since the walk above stopped on it: everything below is
				// accounted for, so the end falls here.
				if (typeof next.held === "object" && later(next.held.started, this.pts)) {
					this.after = next.sequence;
					return this.after;
				}
				this.accounted++;
			} else {
				const below = next ? Math.min(next.sequence, after) : after;
				if (holdings.complete()) this.accounted = below;
				else {
					const gone = holdings.gone(this.accounted);
					if (gone === undefined || gone <= this.accounted) return undefined;
					this.accounted = Math.min(gone, below);
				}
			}
		}
		return this.after;
	}
}

/** Where a resolved range stops. */
type End =
	/** Before this group. */
	| { before: number }
	/** Past every group. */
	| "open"
	/** Before the first group starting after a time, once that is known. */
	| { time: TimeEnd };

/** One non-empty range, resolved. */
type Span = {
	/** Which of the ranges it came from. */
	index: number;
	start: number;
	end: End;
	/** Whether the start names a group, rather than reaching back to the oldest one. */
	floored: boolean;
};

/** The exclusive end of the groups a span reads now, infinity for none: an unsettled time end reads up to its cap. */
function limitOf(span: Span): number {
	if (span.end === "open") return Number.POSITIVE_INFINITY;
	if ("before" in span.end) return span.end.before;
	return span.end.time.limit ?? Number.POSITIVE_INFINITY;
}

/**
 * A subscriber's ranges resolved to the group sequences it reads. Mirrors the Rust `Window`.
 *
 * A relative or time anchor resolves when the window is built, and again only when the ranges
 * are replaced. Before any group exists a relative anchor waits, reading everything, until
 * {@link settle} names the first one. A time start before any group has started waits too, but
 * reading nothing of its range, since a group arrives before the frame that shows where it
 * starts. A time end settles as the groups it waits on start.
 */
export class Window {
	/** What these were resolved from, so only replacing the ranges resolves again. */
	readonly ranges: readonly Range[];
	#spans: Span[] = [];
	/** The groups the spans cover, an unsettled time end through its cap: merged, sorted, and disjoint. */
	#runs: Run[] = [];
	/** A relative anchor waits on a first group, so nothing is filtered yet. */
	#pending = false;
	/**
	 * The ranges, by index, whose time start waits on a group starting, so they read nothing yet,
	 * with the latest group their relative end counts back from.
	 */
	#waiting: { index: number; latest: number | undefined }[] = [];
	/** Nothing is left to resolve as groups arrive and start. */
	#settled = false;

	/** Resolve `ranges`, counting relative anchors back from `latest` and time anchors from `holdings`. */
	constructor(ranges: readonly Range[], latest: number | undefined, holdings: Holdings = UNHELD) {
		this.ranges = ranges;
		this.#place(latest, holdings);
	}

	/** Whether a relative anchor still waits on a first group. */
	get pending(): boolean {
		return this.#pending;
	}

	/** Whether nothing is left to resolve as groups arrive and start. */
	get settled(): boolean {
		return this.#settled;
	}

	/**
	 * Resolve what the groups now held settle: relative anchors once `latest` exists, and time
	 * starts and ends as groups start. Returns whether the groups in range moved.
	 */
	settle(latest: number | undefined, holdings: Holdings = UNHELD): boolean {
		if (this.#pending) {
			if (latest === undefined) return false;
			this.#place(latest, holdings);
			return !this.#pending;
		}
		let moved = false;
		const waiting = this.#waiting;
		this.#waiting = [];
		// A relative end keeps counting back from the latest group it was asked against, not
		// one that arrived while the start waited.
		for (const { index, latest } of waiting) {
			if (this.#span(index, latest, holdings)) moved = true;
		}
		for (const span of this.#spans) {
			if (span.end === "open" || !("time" in span.end)) continue;
			const time = span.end.time;
			const limit = time.limit;
			const end = time.advance(span.start, holdings);
			if (end !== undefined) {
				span.end = { before: end };
				moved = true;
			} else if (time.limit !== limit) moved = true;
		}
		if (moved) this.#rebuild();
		return moved;
	}

	/** Whether group `sequence` is in range. Everything is while a relative anchor waits. */
	contains(sequence: number): boolean {
		if (this.#pending) return true;
		const run = this.#runAt(sequence);
		return run !== undefined && sequence >= run.start;
	}

	/**
	 * Whether a datagram is in range, judged on its own and never through a time end's settling:
	 * its `timestamp` against each range's times, and its `sequence` against the range's absolute
	 * groups and the relative ones resolved so far.
	 */
	admitsDatagram(sequence: number, timestamp: Timestamp | undefined): boolean {
		if (timestamp === undefined) {
			return this.#pending || this.#spans.some((span) => sequence >= span.start && sequence < limitOf(span));
		}
		return this.ranges.some((range, index) => {
			if (isEmpty(range)) return false;
			// A relative anchor still waiting on a latest group bounds nothing yet.
			const span = this.#spans.find((span) => span.index === index);
			const { start, end } = range;
			const started =
				start?.time !== undefined
					? !later(start.time, timestamp)
					: start?.absolute !== undefined
						? sequence >= start.absolute
						: start === undefined || !span || sequence >= span.start;
			const ended =
				end === undefined
					? true
					: end.time !== undefined
						? !later(timestamp, end.time)
						: end.absolute !== undefined
							? sequence < end.absolute
							: !span || span.end === "open" || !("before" in span.end) || sequence < span.end.before;
			return started && ended;
		});
	}

	/** The lowest group in range at or above `from`, undefined when none is. `from` itself while a relative anchor waits. */
	next(from: number): number | undefined {
		if (this.#pending) return from;
		const run = this.#runAt(from);
		return run === undefined ? undefined : Math.max(run.start, from);
	}

	/** The lowest group in range, undefined while an anchor waits. Nothing in range floors at group 0. */
	floor(): number | undefined {
		if (this.#pending || this.#waiting.length > 0) return undefined;
		return this.#runs[0]?.start ?? 0;
	}

	/**
	 * The lowest group the ranges name, undefined while one reaches back to the oldest group or
	 * a relative anchor waits on a first group.
	 */
	start(): number | undefined {
		if (this.#pending || this.#waiting.length > 0 || this.#spans.some((span) => !span.floored)) return undefined;
		let start: number | undefined;
		for (const span of this.#spans) start = Math.min(start ?? span.start, span.start);
		return start;
	}

	/**
	 * The exclusive end past every group in range, undefined when one stays open. An unsettled
	 * time end counts as open: its cap only holds reads back meanwhile. Nothing in range ends at 0.
	 */
	end(): number | undefined {
		if (this.#pending || this.#waiting.length > 0) return undefined;
		if (this.#spans.some((span) => span.end !== "open" && "time" in span.end)) return undefined;
		const end = this.#runs.at(-1)?.end ?? 0;
		return end === Number.POSITIVE_INFINITY ? undefined : end;
	}

	/** The runs in range from `from` up to the exclusive `end`, lowest first. */
	runs(from = 0, end = Number.POSITIVE_INFINITY): Run[] {
		if (from >= end) return [];
		if (this.#pending) return [{ start: from, end }];
		const out: Run[] = [];
		for (const run of this.#runs) {
			if (run.end <= from) continue;
			if (run.start >= end) break;
			out.push({ start: Math.max(run.start, from), end: Math.min(run.end, end) });
		}
		return out;
	}

	/**
	 * Each range resolved, in the order given: undefined for one that is empty or whose start
	 * still waits. An unsettled time end is reported open, never at its cap.
	 */
	resolved(): (Resolved | undefined)[] {
		const resolved: (Resolved | undefined)[] = this.ranges.map(() => undefined);
		for (const span of this.#spans) {
			resolved[span.index] = {
				start: span.start,
				end: span.end !== "open" && "before" in span.end ? span.end.before : undefined,
			};
		}
		return resolved;
	}

	// The first run ending past `sequence`.
	#runAt(sequence: number): Run | undefined {
		let lo = 0;
		let hi = this.#runs.length;
		while (lo < hi) {
			const mid = (lo + hi) >>> 1;
			if (this.#runs[mid].end <= sequence) lo = mid + 1;
			else hi = mid;
		}
		return this.#runs[lo];
	}

	#place(latest: number | undefined, holdings: Holdings): void {
		this.#spans = [];
		this.#pending = false;
		this.#waiting = [];
		for (let index = 0; index < this.ranges.length; index++) this.#span(index, latest, holdings);
		this.#rebuild();
	}

	// Resolve range `index` into a span, or note what it waits on. Returns whether it resolved.
	#span(index: number, latest: number | undefined, holdings: Holdings): boolean {
		const range = this.ranges[index];
		if (isEmpty(range)) return false;
		const start = startOf(range.start, latest, holdings);
		if (start === "waiting") {
			this.#waiting.push({ index, latest });
			return false;
		}
		const until = endOf(range.end, latest);
		if (start === undefined || until === undefined) {
			this.#pending = true;
			return false;
		}
		let end: End = until === "open" || "before" in until ? until : { time: new TimeEnd(until.pts, start.group) };
		if (end !== "open" && "time" in end) {
			const settled = end.time.advance(start.group, holdings);
			if (settled !== undefined) end = { before: settled };
		}
		this.#spans.push({ index, start: start.group, end, floored: start.floored });
		return true;
	}

	#rebuild(): void {
		this.#settled =
			!this.#pending &&
			this.#waiting.length === 0 &&
			this.#spans.every((span) => span.end === "open" || !("time" in span.end));
		const spans = this.#spans
			.map((span) => ({ start: span.start, end: limitOf(span) }))
			.filter((span) => span.end > span.start)
			.sort((a, b) => a.start - b.start);
		this.#runs = [];
		for (const span of spans) {
			const last = this.#runs.at(-1);
			if (last && span.start <= last.end) last.end = Math.max(last.end, span.end);
			else this.#runs.push({ start: span.start, end: span.end });
		}
	}
}

/**
 * The first group a range starts at, and whether it names that group rather than reaching back
 * to the oldest one. Undefined while it waits on a latest group, and `"waiting"` for a time
 * start while no group has started.
 */
function startOf(
	anchor: Anchor | undefined,
	latest: number | undefined,
	holdings: Holdings,
): { group: number; floored: boolean } | "waiting" | undefined {
	const group = absolute(anchor);
	if (group !== undefined) return { group, floored: true };
	const pts = time(anchor);
	if (pts !== undefined) {
		const containing = holdings.containing(pts);
		// The group holding it is older than any held, so reach back to the oldest, as when
		// nothing can start any more.
		if (containing === "before" || (containing === "unknown" && holdings.complete())) {
			return { group: 0, floored: false };
		}
		// Nothing has started yet, so wait for a group that has. A copy may hear the live edge
		// before the groups its publisher resolved the time to, which the edge would skip.
		return containing === "unknown" ? "waiting" : { group: containing.group, floored: true };
	}
	const back = relative(anchor);
	if (back === undefined) return { group: 0, floored: false };
	if (latest === undefined) return undefined;
	return { group: Math.max(0, latest - back), floored: true };
}

/**
 * Where a range stops, exclusive, or undefined while it waits on a latest group. A time end's
 * walk starts once its start is known.
 */
function endOf(
	anchor: Anchor | undefined,
	latest: number | undefined,
): { before: number } | "open" | { pts: Timestamp } | undefined {
	if (anchor === undefined) return "open";
	const end = absolute(anchor);
	if (end !== undefined) return { before: end };
	const pts = time(anchor);
	if (pts !== undefined) return { pts };
	if (latest === undefined) return undefined;
	return { before: Math.max(0, latest - (relative(anchor) ?? 0)) + 1 };
}

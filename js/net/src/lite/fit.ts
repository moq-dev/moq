// Widening a lite-07 subscription's ranges until they fit the message that carries them. Mirrors
// the Rust `lite/subscriber/fit.rs`.
//
// A relay asks its upstream for the union of every viewer's ranges, and each viewer's request
// fits one message while the union need not. Rather than give back the copy every viewer reads
// through, the relay asks for a covering superset: the narrowest gaps close first, so the fewest
// unwanted groups are asked for. Each viewer still reads only its own ranges from the copy.

import type { Run } from "../internal.ts";
import { max } from "../util/key.ts";
import { MAX_MESSAGE_SIZE } from "./message.ts";
import { rangeSize, varintSize, type WireRange } from "./subscribe.ts";

/**
 * The most a SUBSCRIBE_UPDATE's fields besides its ranges take, size prefix included: nine
 * varints of at most nine bytes, and the priority byte.
 */
export const UPDATE_FIELDS = 9 * 9 + 1;

/** What lite-07 ranges may take on the wire. */
export interface Budget {
	/** The bytes every range and its Range ID may take together. */
	bytes: number;
	/** The bytes the largest Range ID any of the ranges may take. */
	id: number;
}

/**
 * The budget for ranges a subscription takes Range IDs from `next` for, whose SUBSCRIBE carries
 * `fields` bytes besides its ranges, `count` the most ranges it asks for.
 *
 * Half a message, so that the ranges sent last, retired by Range ID, and the ranges replacing
 * them fit one SUBSCRIBE_UPDATE however little the two share: a relay never narrows its ask in
 * between, which would reset group streams still arriving.
 */
export function budget(fields: number, next: bigint, count: number): Budget {
	const half = Math.floor((MAX_MESSAGE_SIZE - UPDATE_FIELDS) / 2);
	const largest = next + BigInt(count);
	return {
		bytes: Math.min(half, Math.max(0, MAX_MESSAGE_SIZE - fields)),
		// Past the varint's reach it takes the most a varint can.
		id: largest >= 1n << 64n ? 9 : varintSize(largest),
	};
}

/**
 * The longest prefix of `ranges` fitting half of what `budget` has left, which it then takes: the
 * other half stays for the demand and fills, which {@link fit} can only widen so far. Mirrors the
 * Rust `Budget::take`.
 */
export function take(budget: Budget, ranges: readonly WireRange[]): WireRange[] {
	const room = Math.floor(budget.bytes / 2);
	let used = 0;
	const taken: WireRange[] = [];
	for (const range of ranges) {
		const bytes = cost(budget, range);
		if (bytes === undefined || used + bytes > room) break;
		used += bytes;
		taken.push(range);
	}
	budget.bytes -= used;
	return taken;
}

/** What `range` costs, or undefined when it can't go out. */
function cost(budget: Budget, range: WireRange): number | undefined {
	// An end at group 0 asks for nothing, which encoding refuses.
	if (range.end?.absolute === 0n) return undefined;
	try {
		return rangeSize(range) + budget.id;
	} catch {
		// Past the varint's reach, which encoding refuses too.
		return undefined;
	}
}

/** Groups from `start`, up to the exclusive `end` or open. */
interface Span {
	start: bigint;
	end?: bigint;
}

function spanCost(budget: Budget, span: Span): number {
	const range: WireRange = { start: { absolute: span.start } };
	if (span.end !== undefined) range.end = { absolute: span.end };
	return cost(budget, range) ?? Number.POSITIVE_INFINITY;
}

/**
 * The groups `range` asks for when it starts at one. A time is a group's start, inclusive as an
 * end; any other end is taken as open.
 */
function groupSpan(range: WireRange): Span | undefined {
	const start = range.start?.absolute ?? range.start?.time;
	if (start === undefined) return undefined;
	const end = range.end?.absolute ?? (range.end?.time === undefined ? undefined : range.end.time + 1n);
	return end === undefined ? { start } : { start, end };
}

/**
 * `demand`'s ranges and the `fills` groups, each widened only as far as both must be to fit
 * `budget`, as the same arrays when nothing must. A gap between fills closes across `avoid`,
 * groups the fills leave out on purpose, only once every other gap has. A widened demand range
 * names its groups by time, as the demand's own do.
 *
 * Throws when even one range per kind of start would not fit, which only a broadcast path or
 * track name filling the message leaves.
 */
export function fit(
	demand: readonly WireRange[],
	fills: readonly Run[],
	avoid: readonly Run[],
	budget: Budget,
): { ranges: readonly WireRange[]; fills: readonly Run[] } {
	let sum = 0;
	for (const range of [...demand, ...fills.map(fillRange)]) {
		const bytes = cost(budget, range);
		// One that can't go out fails when it is encoded, as it would unfitted.
		if (bytes === undefined) return { ranges: demand, fills };
		sum += bytes;
	}
	if (sum <= budget.bytes) return { ranges: demand, fills };

	const starts: Span[] = [];
	let others: WireRange[] = [];
	for (const range of demand) {
		const span = groupSpan(range);
		if (span) starts.push(span);
		else others.push(range);
	}
	const spans = new Spans(starts, budget, () => false);
	const runs = new Spans(
		fills.map((run) => ({ ...run })),
		budget,
		(gap) => avoid.some((run) => run.start < gap.end && gap.start < run.end),
	);

	// The other ranges count back from the latest group or start at the oldest, so no gap between
	// them is measured: when even one span per list leaves them no room, each kind collapses to
	// the one range covering it.
	const costOf = (ranges: readonly WireRange[]) =>
		ranges.reduce((sum, range) => sum + (cost(budget, range) ?? Number.POSITIVE_INFINITY), 0);
	if (costOf(others) + spans.least() + runs.least() > budget.bytes) others = collapse(others);
	const rest = costOf(others);
	const total = () => rest + spans.total + runs.total;

	// Close the narrowest gaps first, of either list, until everything fits.
	const gaps = [...spans.gaps(false), ...runs.gaps(true)].sort(byGap);
	for (const gap of gaps) {
		if (total() <= budget.bytes) break;
		(gap.fill ? runs : spans).join(gap.index);
	}
	if (total() > budget.bytes) throw new Error("too many ranges for one SUBSCRIBE");

	const ranges: WireRange[] = spans.ranges().map(({ start, end }) => {
		const range: WireRange = { start: { time: start } };
		if (end !== undefined) range.end = { time: end - 1n };
		return range;
	});
	ranges.push(...others);
	return { ranges, fills: runs.ranges().map(({ start, end }) => ({ start, end: bounded(end) })) };
}

/** The range a fill's run takes on the wire, as `RangeFills` asks it. */
function fillRange(run: Run): WireRange {
	return { start: { absolute: run.start }, end: { absolute: run.end } };
}

/** A gap's end: only the last span is open, and no gap follows it. */
function bounded(end: bigint | undefined): bigint {
	if (end === undefined) throw new Error("only the last span is open");
	return end;
}

/** A gap between two spans: how late it closes, which list it is in, and the span before it. */
interface Gap {
	late: boolean;
	width: bigint;
	fill: boolean;
	index: number;
}

function byGap(a: Gap, b: Gap): number {
	if (a.late !== b.late) return a.late ? 1 : -1;
	if (a.width !== b.width) return a.width < b.width ? -1 : 1;
	if (a.fill !== b.fill) return a.fill ? 1 : -1;
	return a.index - b.index;
}

/** Disjoint spans in order, each the run of the input spans from one start to its chain's end. */
class Spans {
	readonly #spans: Span[] = [];
	// The last span of the chain starting at each chain's first, and the first of the chain ending
	// at each chain's last.
	readonly #end: number[];
	readonly #start: number[];
	// Whether the gap after each span closes only once every other gap has.
	readonly #late: boolean[] = [];
	readonly #budget: Budget;
	total = 0;

	constructor(input: Span[], budget: Budget, late: (gap: Run) => boolean) {
		this.#budget = budget;
		input.sort(bySpan);
		// Spans that overlap or touch cost less as one, and ask for nothing more.
		for (const span of input) {
			const last = this.#spans.at(-1);
			if (last && (last.end === undefined || last.end >= span.start)) {
				last.end = last.end === undefined || span.end === undefined ? undefined : max(last.end, span.end);
			} else {
				this.#spans.push({ ...span });
			}
		}
		for (let index = 0; index + 1 < this.#spans.length; index++)
			this.#late.push(late({ start: bounded(this.#spans[index].end), end: this.#spans[index + 1].start }));
		for (const span of this.#spans) this.total += spanCost(budget, span);
		this.#end = this.#spans.map((_, index) => index);
		this.#start = this.#spans.map((_, index) => index);
	}

	/** What the one span covering every span costs, or nothing when there are none. */
	least(): number {
		const first = this.#spans.at(0);
		const last = this.#spans.at(-1);
		if (!first || !last) return 0;
		return spanCost(this.#budget, { start: first.start, end: last.end });
	}

	/** Every gap, tagged with whether this list is the fills'. */
	gaps(fill: boolean): Gap[] {
		return this.#late.map((late, index) => ({
			late,
			width: this.#spans[index + 1].start - bounded(this.#spans[index].end),
			fill,
			index,
		}));
	}

	/** Close the gap after span `index`, joining the chains on either side. */
	join(index: number): void {
		const first = this.#start[index];
		const last = this.#end[index + 1];
		const spans = this.#spans;
		this.total -= spanCost(this.#budget, { start: spans[first].start, end: spans[index].end });
		this.total -= spanCost(this.#budget, { start: spans[index + 1].start, end: spans[last].end });
		this.total += spanCost(this.#budget, { start: spans[first].start, end: spans[last].end });
		this.#end[first] = last;
		this.#start[last] = first;
	}

	/** Each chain as the one span covering it. */
	ranges(): Span[] {
		const ranges: Span[] = [];
		for (let first = 0; first < this.#spans.length; first = this.#end[first] + 1) {
			const last = this.#end[first];
			ranges.push({ start: this.#spans[first].start, end: this.#spans[last].end });
		}
		return ranges;
	}
}

/** By start, an open end before any bound one, then by end. */
function bySpan(a: Span, b: Span): number {
	if (a.start !== b.start) return a.start < b.start ? -1 : 1;
	if (a.end === b.end) return 0;
	if (a.end === undefined) return -1;
	if (b.end === undefined) return 1;
	return a.end < b.end ? -1 : 1;
}

/**
 * One range per kind of start covering every range of that kind: from the earliest start to the
 * latest end. A time or absolute start is a span, so only the oldest and counts back are left.
 */
function collapse(ranges: readonly WireRange[]): WireRange[] {
	const kinds: WireRange[] = [];
	for (const range of ranges) {
		const relative = range.start?.relative;
		const cover = kinds.find((cover) => (cover.start?.relative === undefined) === (relative === undefined));
		if (!cover) {
			kinds.push({ ...range });
			continue;
		}
		if (cover.start?.relative !== undefined && relative !== undefined)
			cover.start = { relative: max(cover.start.relative, relative) };
		const [a, b] = [cover.end, range.end];
		if (a?.absolute !== undefined && b?.absolute !== undefined)
			cover.end = { absolute: max(a.absolute, b.absolute) };
		else if (a?.time !== undefined && b?.time !== undefined) cover.end = { time: max(a.time, b.time) };
		// Ends of different kinds can't be ordered without the latest group: follow the edge.
		else delete cover.end;
	}
	return kinds;
}

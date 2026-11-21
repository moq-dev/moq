import { fromTransport, ProtocolViolation, StreamCode, StreamError, toStreamCode } from "../error.ts";
import type * as Instance from "../instance.ts";
import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import type { Order, Position, Range } from "../track.ts";
import { UNBOUNDED } from "../util/key.ts";
import { lengthLeadingOnes, parts, split } from "../util/varint.ts";
import { EMPTY } from "../window.ts";
import * as Duration from "./duration.ts";
import { decodeNode, decodePin, encodeNode, encodePin } from "./instance.ts";
import * as Message from "./message.ts";
import { fromWire, toWire } from "./priority.ts";
import {
	hasDropCode,
	hasEndError,
	hasFrameBounds,
	hasGroupOrder,
	hasGroupSpan,
	hasMinDelay,
	hasOrder,
	hasRanges,
	hasRefresh,
	hasSkip,
	hasUnlimitedMaxDelay,
	resolvesStart,
	Version,
} from "./version.ts";

/**
 * Encode the `Group Start` field shared by SUBSCRIBE and SUBSCRIBE_UPDATE.
 *
 * Lite-06 writes the raw floor (`undefined` and 0 are both group 0). A pre-06 wire
 * encodes the key + 1: omitting the floor is 0 (the latest group, where the
 * publisher starts) and an explicit 0 is 1, replay from the beginning.
 */
async function encodeStartGroup(w: Writer, version: Version, startGroup?: bigint) {
	if (resolvesStart(version)) {
		await w.u62(startGroup ?? 0n);
		return;
	}
	await w.u62(startGroup === undefined ? 0n : startGroup + 1n);
}

/**
 * Decode the `Group Start` field shared by SUBSCRIBE and SUBSCRIBE_UPDATE.
 *
 * The inverse of {@link encodeStartGroup}. Callers canonicalize with
 * {@link canonicalStartGroup} once the frame bounds are known.
 */
async function decodeStartGroup(r: Reader, version: Version): Promise<bigint | undefined> {
	const value = await r.u62();
	if (resolvesStart(version)) return value;
	return value > 0n ? value - 1n : undefined;
}

/**
 * Canonicalize a decoded floor: a lite-06 `Group Start` of 0 with no frame offset is the
 * same absence of a constraint as no floor at all, so it decodes as undefined. Group 0
 * stays named only when a `Frame Start` actually qualifies it (a subscription can resume
 * partway through group 0: a catalog never leaves it).
 */
function canonicalStartGroup(version: Version, startGroup: bigint | undefined, startFrame: number): bigint | undefined {
	if (resolvesStart(version) && startGroup === 0n && startFrame === 0) return undefined;
	return startGroup;
}

/**
 * Encode the trailing `Frame Start` / `Frame End` pair shared by SUBSCRIBE and
 * SUBSCRIBE_UPDATE. A no-op before lite-06, which has nowhere to put them.
 */
async function encodeFrameBounds(
	w: Writer,
	version: Version,
	{
		startGroup,
		startFrame,
		endGroup,
		endFrame,
	}: { startGroup?: bigint; startFrame: number; endGroup?: bigint; endFrame?: number },
) {
	if ((startFrame !== 0 && startGroup === undefined) || (endFrame !== undefined && endGroup === undefined)) {
		throw new Error("frame bound without a group bound");
	}

	if (!hasFrameBounds(version)) {
		// Silently widening to the whole group would deliver frames we excluded.
		if (startFrame !== 0 || endFrame !== undefined) {
			throw new Error("frame bounds not supported for this version");
		}
		return;
	}

	await w.u53(startFrame);
	await w.u53(endFrame !== undefined ? endFrame + 1 : 0);
}

/**
 * Decode the trailing `Frame Start` / `Frame End` pair, defaulting to the whole group.
 *
 * A frame bound without the group bound it qualifies is a protocol violation: frames
 * are numbered per group, so there is nothing to count from.
 */
async function decodeFrameBounds(
	r: Reader,
	version: Version,
	startGroup?: bigint,
	endGroup?: bigint,
): Promise<{ startFrame: number; endFrame?: number }> {
	if (!hasFrameBounds(version)) {
		return { startFrame: 0 };
	}

	const startFrame = await r.u53();
	const endFrame = await r.u53();

	if ((startFrame !== 0 && startGroup === undefined) || (endFrame !== 0 && endGroup === undefined)) {
		throw new Error("frame bound without a group bound");
	}

	return { startFrame, endFrame: endFrame > 0 ? endFrame - 1 : undefined };
}

/** The span SUBSCRIBE and SUBSCRIBE_UPDATE carry, with inclusive ends. */
interface Span {
	startGroup?: bigint;
	startFrame: number;
	endGroup?: bigint;
	endFrame?: number;
}

/**
 * One end of a lite-07 range: a model time (`time`), or a group by its start (`absolute`), as the
 * Rust `lite::Anchor` holds. A group's start is its time, so `absolute` goes out as a time, an end
 * (exclusive) as the time just before it, and decodes as one. A start partway through the group,
 * at a nonzero `frame`, goes out as a Position; an end there can't.
 */
export type WireAnchor = { absolute: bigint; frame?: number; time?: never } | { time: bigint; absolute?: never };

/** A lite-07 range as the wire carries it; see {@link WireAnchor}. */
export interface WireRange {
	start?: WireAnchor;
	end?: WireAnchor;
}

/** The wire form of model `ranges`: the same times, a time being a group start. */
export function toWireRanges(ranges: readonly Range[]): WireRange[] {
	return ranges.map((range) => {
		const out: WireRange = {};
		if (range.start !== undefined) out.start = { time: range.start };
		if (range.end !== undefined) out.end = { time: range.end };
		return out;
	});
}

/**
 * The lite-07 Anchor Type and values of a range's start or `end`: None (0), Time (1), or Position
 * (2). Both time ends are inclusive, so an absolute end, exclusive, goes out as the time just
 * before it; one at group 0 holds nothing a time can say. A start partway through a group is a
 * Position: its start, then the frame.
 */
function wireAnchor(anchor: WireAnchor | undefined, end: boolean): [number, ...bigint[]] {
	if (anchor?.absolute !== undefined) {
		if (anchor.frame) {
			if (end) throw new Error("a range ends only at a group");
			return [2, anchor.absolute, BigInt(anchor.frame)];
		}
		if (!end) return [1, anchor.absolute];
		if (anchor.absolute === 0n) throw new Error(EMPTY_RANGE);
		return [1, anchor.absolute - 1n];
	}
	if (anchor?.time !== undefined) return [1, anchor.time];
	return [0];
}

/** Encode one lite-07 range, its start then its end; see {@link wireAnchor}. */
async function encodeRange(w: Writer, range: WireRange) {
	for (const [anchor, end] of [
		[range.start, false],
		[range.end, true],
	] as const) {
		const [kind, ...values] = wireAnchor(anchor, end);
		await w.u53(kind);
		for (const value of values) await w.u62(value);
	}
}

/**
 * Encode the span after the max delay. Before lite-07 that is `Group Start`, `Group End`,
 * and the frame bounds; lite-07 carries a list of ranges, which is `ranges` when given and
 * otherwise this span as its only one.
 */
async function encodeSpan(w: Writer, version: Version, span: Span, ranges?: readonly WireRange[]) {
	if (ranges !== undefined && hasRanges(version)) {
		if (ranges.length === 0) throw new Error(EMPTY_RANGE);
		await w.u53(ranges.length);
		for (const range of ranges) await encodeRange(w, range);
		return;
	}
	if (!hasRanges(version)) {
		await encodeStartGroup(w, version, span.startGroup);
		await w.u62(span.endGroup !== undefined ? span.endGroup + 1n : 0n);
		await encodeFrameBounds(w, version, span);
		return;
	}

	// A lite-07 range starts partway through a group as a Position, but ends only at one: a
	// slice of a group is a FETCH.
	if (span.endFrame !== undefined || (span.startFrame !== 0 && span.startGroup === undefined))
		throw new Error("frame bounds not supported for this version");
	await w.u53(1);
	const range: WireRange = {};
	if (span.startGroup !== undefined)
		range.start = span.startFrame
			? { absolute: span.startGroup, frame: span.startFrame }
			: { time: span.startGroup };
	if (span.endGroup !== undefined) range.end = { time: span.endGroup };
	await encodeRange(w, range);
}

/**
 * The model ranges a decoded SUBSCRIBE or SUBSCRIBE_UPDATE asks for: its lite-07 list, or the one
 * span an older wire carries, whole groups either way. Mirrors the Rust `Bounds::new`.
 *
 * A group ID is a group's start, so an absolute anchor and a time anchor both name one: a time
 * start resolves to the group containing it. An older wire means its start as the first group
 * starting at or after it, which the publisher's floor keeps (see `cursorFloor`).
 */
export function requestedRanges(msg: Span & { ranges?: WireRange[] }): Range[] {
	if (msg.ranges) return msg.ranges.map(requestedRange);
	const range: Range = {};
	if (msg.startGroup !== undefined) range.start = msg.startGroup;
	if (msg.endGroup !== undefined) range.end = msg.endGroup;
	return [range];
}

/** The model range one lite-07 range asks for; see {@link requestedRanges}. */
export function requestedRange(range: WireRange): Range {
	// Empty, as group bounds say it: keep its place, covering no group.
	const start = range.start?.absolute;
	const end = range.end?.absolute;
	if (start !== undefined && end !== undefined && end <= start) return { ...EMPTY };
	if (end === 0n) return { ...EMPTY };
	const out: Range = {};
	if (range.start) out.start = start ?? range.start.time;
	// An absolute end is exclusive; every model end includes its group.
	if (range.end) out.end = end === undefined ? range.end.time : end - 1n;
	return out;
}

/**
 * Decode one lite-07 range end, `undefined` for none: a time, inclusive as an end, or a start
 * partway through a group, never at frame 0, which a time says. Any other Anchor Type is a
 * protocol violation.
 */
async function decodeAnchor(r: Reader, end: boolean): Promise<WireAnchor | undefined> {
	const kind = await r.u53();
	switch (kind) {
		case 0:
			return undefined;
		case 1:
			return { time: await r.u62() };
		case 2: {
			if (end) throw new ProtocolViolation("a range ends partway through a group");
			const absolute = await r.u62();
			const frame = await r.u53();
			if (frame === 0) throw new ProtocolViolation("a position at frame 0");
			return { absolute, frame };
		}
		default:
			throw new ProtocolViolation(`unknown range anchor: ${kind}`);
	}
}

/** Decode one lite-07 range: its start, then its end. */
async function decodeRange(r: Reader): Promise<WireRange> {
	const start = await decodeAnchor(r, false);
	const end = await decodeAnchor(r, true);
	const range: WireRange = {};
	if (start) range.start = start;
	if (end) range.end = end;
	return range;
}

/** Decode a lite-07 `Range Count` and its ranges, which may be none. */
async function decodeRanges(r: Reader): Promise<WireRange[]> {
	// Not preallocated: each range costs at least two bytes of the bounded message.
	const ranges: WireRange[] = [];
	for (let count = await r.u53(); count > 0; count--) ranges.push(await decodeRange(r));
	return ranges;
}

/**
 * Lite-07 `ranges` and the one span covering every one of them, which the subscriber narrows
 * again. A range covers whole groups but for a Position start (see {@link positionFrames}),
 * whose frame the span keeps when the span starts there. The publisher reads every Position
 * from the ranges themselves.
 */
export function spanOf(version: Version, ranges: readonly WireRange[]): Span & { ranges: WireRange[] } {
	let start: bigint | undefined;
	let end: bigint | undefined;
	let floor = true;
	let live = false;
	for (const range of ranges) {
		const [from, to] = rangeBounds(range);
		if (from === undefined) floor = false;
		else if (start === undefined || from < start) start = from;
		if (to === undefined) live = true;
		else if (end === undefined || end < to) end = to;
	}
	start = floor ? start : undefined;
	const startFrame = start === undefined ? 0 : (positionFrames(ranges).get(start) ?? 0);
	return { ranges: [...ranges], ...spanBetween(version, start, live ? undefined : end, startFrame) };
}

/**
 * The group a lite-07 range starts at and the exclusive group it ends before, either undefined
 * when it has none: a time is the start of the group it names, inclusive as an end.
 */
export function rangeBounds(range: WireRange): [bigint | undefined, bigint | undefined] {
	const from = range.start?.absolute ?? range.start?.time;
	const to = range.end?.absolute ?? (range.end?.time === undefined ? undefined : range.end.time + 1n);
	return [from, to];
}

/**
 * The span from group `start`, at frame `startFrame` of it, up to the exclusive group `end`, either
 * group undefined for none.
 */
export function spanBetween(
	version: Version,
	start: bigint | undefined,
	end: bigint | undefined,
	startFrame = 0,
): Span {
	// An exclusive end at the very first group names nothing.
	if (end === 0n) throw new ProtocolViolation(EMPTY_RANGE);
	return {
		startGroup: canonicalStartGroup(version, start, startFrame),
		startFrame,
		endGroup: end === undefined ? undefined : end - 1n,
	};
}

/**
 * The first frame each lite-07 Position start asks its group from, by group, for each group no
 * other range may cover: a group is delivered once, so one another range covers goes whole.
 * A peer may send thousands of ranges, so this sorts their bounds once rather than scanning
 * them per Position.
 */
export function positionFrames(ranges: readonly WireRange[]): Map<bigint, number> {
	const frames = new Map<bigint, number>();
	const positions: [bigint, number][] = [];
	for (const range of ranges) {
		const start = range.start;
		// One ending at or before the group it starts partway through covers nothing.
		const [, to] = rangeBounds(range);
		if (start?.absolute !== undefined && start.frame && (to === undefined || to > start.absolute))
			positions.push([start.absolute, start.frame]);
	}
	if (positions.length === 0) return frames;
	// A range may cover a group unless it starts past it or ends at or before it. One that
	// starts at or past its end covers none, so with it left out, no range does both and the
	// ranges covering a group are the rest less those two counts.
	const starts: bigint[] = [];
	const ends: bigint[] = [];
	let count = 0;
	for (const range of ranges) {
		const from = range.start?.absolute ?? range.start?.time;
		// No start, or no end, may cover it.
		const to = range.end?.absolute ?? (range.end?.time === undefined ? undefined : range.end.time + 1n);
		if (from !== undefined && to !== undefined && from >= to) continue;
		count++;
		if (from !== undefined) starts.push(from);
		if (to !== undefined) ends.push(to);
	}
	const order = (a: bigint, b: bigint) => (a < b ? -1 : a > b ? 1 : 0);
	starts.sort(order);
	ends.sort(order);
	for (const [group, frame] of positions) {
		const past = starts.length - countAtOrBelow(starts, group);
		const before = countAtOrBelow(ends, group);
		if (count - past - before === 1) frames.set(group, frame);
	}
	return frames;
}

/** How many of the `sorted` values are at or below `value`. */
function countAtOrBelow(sorted: readonly bigint[], value: bigint): number {
	let low = 0;
	let high = sorted.length;
	while (low < high) {
		const mid = (low + high) >> 1;
		if (sorted[mid] <= value) low = mid + 1;
		else high = mid;
	}
	return low;
}

/**
 * The inverse of {@link encodeSpan}. A lite-07 list decodes as its ranges and as the one span
 * covering every range in it, which the subscriber narrows again.
 */
async function decodeSpan(r: Reader, version: Version): Promise<Span & { ranges?: WireRange[] }> {
	if (!hasRanges(version)) {
		const startGroup = await decodeStartGroup(r, version);
		const endGroup = (await r.u62()) || undefined;
		const end = endGroup !== undefined ? endGroup - 1n : undefined;
		const frames = await decodeFrameBounds(r, version, startGroup, end);
		return { startGroup: canonicalStartGroup(version, startGroup, frames.startFrame), endGroup: end, ...frames };
	}

	const ranges = await decodeRanges(r);
	if (ranges.length === 0) throw new ProtocolViolation("a subscription asks for no range");
	return spanOf(version, ranges);
}

/**
 * The ranges `msg` carries on lite-07: its list, or else the one span it encodes as its only
 * range, in whole groups.
 */
export function rangesOf(msg: Span & { ranges?: WireRange[] }): WireRange[] {
	if (msg.ranges) return msg.ranges;
	const range: WireRange = {};
	if (msg.startGroup !== undefined) range.start = { absolute: msg.startGroup };
	if (msg.endGroup !== undefined) range.end = { absolute: msg.endGroup + 1n };
	return [range];
}

/** The bytes `value` takes as a lite-07 varint, which counts leading ones. */
export function varintSize(value: number | bigint): number {
	const lo = split(value);
	return lengthLeadingOnes(parts.hi, lo);
}

/** The bytes a lite-07 range takes on the wire, whose varints count leading ones. */
export function rangeSize(range: WireRange): number {
	const size = varintSize;
	const anchor = (anchor: WireAnchor | undefined, end: boolean): number => {
		// An end at group 0 can't go out, which encoding refuses; size it as the smallest.
		if (end && anchor?.absolute === 0n) return size(1) + size(0);
		const [kind, ...values] = wireAnchor(anchor, end);
		return values.reduce((total, value) => total + size(value), size(kind));
	};
	return anchor(range.start, false) + anchor(range.end, true);
}

/** A range's anchors as a key, so equal ranges compare equal. */
function rangeKey(range: WireRange): string {
	const anchor = (anchor: WireAnchor | undefined) => {
		if (anchor?.absolute !== undefined) return `a${anchor.absolute}.${anchor.frame ?? 0}`;
		if (anchor?.time !== undefined) return `t${anchor.time}`;
		return "-";
	};
	return `${anchor(range.start)}:${anchor(range.end)}`;
}

/** A received update's ranges resolved by Range ID: the ones retired, then each one added. Mirrors the Rust `Applied`. */
export interface Applied {
	retired: bigint[];
	added: [bigint, WireRange][];
}

/**
 * A subscription's live ranges by Range ID, which both ends of a Subscribe Stream keep: the
 * subscriber to write each update as an edit, the publisher to resolve one. Mirrors the Rust
 * `LiveRanges`.
 *
 * The SUBSCRIBE's ranges take Range IDs from 0 in order, and each range an update adds takes the
 * next. Before lite-07 an update replaces the list, numbered afresh.
 */
export class LiveRanges {
	readonly #version: Version;
	#next = 0n;
	// In Range ID order, since each id is the next.
	readonly #live = new Map<bigint, WireRange>();
	// The bytes the live ranges take on the wire, held to what one SUBSCRIBE carries.
	#size = 0;
	/** How many ranges the SUBSCRIBE carried, whose starts SUBSCRIBE_START reports. */
	readonly initial: bigint;

	/** The ranges a SUBSCRIBE carried. */
	constructor(version: Version, ranges: readonly WireRange[]) {
		this.#version = version;
		this.initial = BigInt(ranges.length);
		this.#add(ranges);
	}

	#add(ranges: readonly WireRange[]) {
		for (const range of ranges) {
			// Only lite-07 numbers ranges on the wire, and only it is held to one SUBSCRIBE.
			if (hasRanges(this.#version)) this.#size += rangeSize(range);
			this.#live.set(this.#next++, range);
		}
	}

	#remove(id: bigint): boolean {
		const range = this.#live.get(id);
		if (!range) return false;
		if (hasRanges(this.#version)) this.#size -= rangeSize(range);
		this.#live.delete(id);
		return true;
	}

	/** The Range ID the next range added takes. */
	next(): bigint {
		return this.#next;
	}

	/** The live range with Range ID `id`. */
	get(id: bigint): WireRange | undefined {
		return this.#live.get(id);
	}

	/** Whether `id` was ever assigned. */
	assigned(id: bigint): boolean {
		return id < this.#next;
	}

	/** Every live Range ID, in order. */
	ids(): bigint[] {
		return [...this.#live.keys()];
	}

	/** Every live range, in Range ID order. */
	ranges(): WireRange[] {
		return [...this.#live.values()];
	}

	/** Every live range with its Range ID, in order. */
	entries(): [bigint, WireRange][] {
		return [...this.#live];
	}

	/**
	 * Apply a received lite-07 `update`'s edit, returning it by Range ID: the ranges retired, then
	 * each added with the Range ID it took, costing the ranges it names. Throws
	 * {@link ProtocolViolation} for an edit it cannot apply. Before lite-07 an update restates its
	 * ranges, so nothing is numbered and the edit is empty. Mirrors the Rust `LiveRanges::apply`.
	 */
	apply(update: SubscribeUpdate): Applied {
		if (!hasRanges(this.#version)) return { retired: [], added: [] };
		const retired = update.remove ?? [];
		for (const id of retired) {
			// An id never assigned, or already retired (including earlier in this list).
			if (!this.#remove(id)) throw new ProtocolViolation(`unknown range id: ${id}`);
		}
		const first = this.#next;
		const add = update.ranges ?? [];
		this.#add(add);
		// Asking for nothing is refused like a SUBSCRIBE that does, and a set no SUBSCRIBE
		// could carry like one too large to send.
		if (this.#live.size === 0) throw new ProtocolViolation("a subscription asks for no range");
		if (this.#size > Message.MAX_MESSAGE_SIZE) throw new ProtocolViolation("too many ranges");
		return { retired: [...retired], added: add.map((range, index) => [first + BigInt(index), range]) };
	}

	/**
	 * The edit that turns the live ranges into `ranges`, which become the live ones: the Range IDs
	 * to retire and the ranges to add. A range already live keeps its Range ID, so an update that
	 * changes one range sends only that one, and the publisher keeps where the rest resolved.
	 * Before lite-07 replaced ranges take fresh Range IDs.
	 *
	 * The last `fills` of `ranges` ask for fills, and `isFill` names the live ones that do: a fill
	 * asking for what a demand range does still takes a range of its own, so each Range ID stays
	 * the demand's or a fill's. `fillIds` are the Range IDs the added fill ranges took. The live
	 * ranges in `renew` are retired and added again even when wanted, so the publisher resolves
	 * them afresh.
	 */
	edit(
		ranges: readonly WireRange[],
		fills = 0,
		isFill: (id: bigint) => boolean = () => false,
		renew: ReadonlySet<bigint> = new Set(),
	): { remove: bigint[]; add: WireRange[]; fillIds: bigint[] } {
		if (ranges.length === 0) throw new Error(EMPTY_RANGE);
		if (!hasRanges(this.#version)) {
			const same =
				ranges.length === this.#live.size &&
				this.ranges().every((range, index) => rangeKey(range) === rangeKey(ranges[index]));
			if (!same) {
				this.#live.clear();
				this.#add(ranges);
			}
			return { remove: [], add: [...ranges], fillIds: [] };
		}
		const demand = ranges.length - fills;
		const key = (range: WireRange, fill: boolean) => `${fill ? "f" : "d"}${rangeKey(range)}`;
		// Each live range keeps at most one wanted copy, so duplicates edit like any other.
		const wanted = new Map<string, number>();
		ranges.forEach((range, index) => {
			const k = key(range, index >= demand);
			wanted.set(k, (wanted.get(k) ?? 0) + 1);
		});
		const remove: bigint[] = [];
		// The bytes the live ranges take once the edit applies.
		let size = this.#size;
		for (const [id, range] of this.#live) {
			const k = key(range, isFill(id));
			const count = wanted.get(k) ?? 0;
			if (count > 0 && !renew.has(id)) {
				wanted.set(k, count - 1);
			} else {
				remove.push(id);
				size -= rangeSize(range);
			}
		}
		// In the order asked for, so the publisher numbers them the way this side does.
		const add: WireRange[] = [];
		const fillIds: bigint[] = [];
		const first = this.#next;
		ranges.forEach((range, index) => {
			const k = key(range, index >= demand);
			const count = wanted.get(k) ?? 0;
			if (count === 0) return;
			wanted.set(k, count - 1);
			add.push(range);
			if (index >= demand) fillIds.push(first + BigInt(add.length - 1));
		});
		// Checked before anything changes, so a refused edit leaves the ranges as last sent.
		for (const range of add) size += rangeSize(range);
		if (size > Message.MAX_MESSAGE_SIZE) throw new Error("too many ranges for one SUBSCRIBE");
		for (const id of remove) this.#remove(id);
		this.#add(add);
		return { remove, add, fillIds };
	}
}

/** Step over the retired `Ordered` byte on a version whose layout still has it. */
async function skipGroupOrder(r: Reader, version: Version) {
	if (hasGroupOrder(version)) await r.bool();
}

/** Write the retired `Ordered` byte as 0, keeping a deployed version's field offsets. */
async function padGroupOrder(w: Writer, version: Version) {
	if (hasGroupOrder(version)) await w.bool(false);
}

/**
 * Decode the byte after the subscriber priority shared by SUBSCRIBE and SUBSCRIBE_UPDATE:
 * lite-07's `Group Order`, or the retired `Ordered` byte an older layout still carries.
 */
async function decodeOrder(r: Reader, version: Version): Promise<Order> {
	if (!hasOrder(version)) {
		await skipGroupOrder(r, version);
		return "descending";
	}
	const order = await r.u8();
	switch (order) {
		case 0:
			return "descending";
		case 1:
			return "ascending";
		default:
			throw new ProtocolViolation(`unknown group order: ${order}`);
	}
}

/**
 * The inverse of {@link decodeOrder}. A wire without `Group Order` serves newest first, so an
 * ascending subscription over it falls back to descending.
 */
async function encodeOrder(w: Writer, version: Version, order: Order) {
	if (!hasOrder(version)) {
		await padGroupOrder(w, version);
		return;
	}
	await w.u8(order === "ascending" ? 1 : 0);
}

/** The error for a requested range the wire cannot carry: one asking for nothing. */
export const EMPTY_RANGE = "empty subscription range cannot be encoded";

/**
 * Inclusive last group a Subscribe message carries, from an exclusive model end.
 *
 * Empty (`0`) cannot be encoded: the wire's 0 means unbounded. Callers refuse an empty
 * requested range before reaching here.
 */
export function inclusiveGroupEnd(exclusive?: bigint): bigint | undefined {
	if (exclusive === undefined) return undefined;
	if (exclusive === 0n) throw new Error(EMPTY_RANGE);
	return exclusive - 1n;
}

/** Encode the `Subscriber Min Age`: milliseconds plus one, zero meaning not stated. Dropped
 * on a wire without the field. */
async function encodeMinDelay(w: Writer, version: Version, minDelay: number | undefined) {
	if (!hasMinDelay(version)) return;
	await Duration.encodeOptional(w, minDelay);
}

/** What lite-03 to lite-06 send for no `Subscriber Max Age` limit, which their bare milliseconds can't say. */
const MAX_SAFE_DELAY_MS = Number.MAX_SAFE_INTEGER;

/**
 * Encode the `Subscriber Max Age`, `null` for no limit: on lite-07 milliseconds plus one, 0 for
 * no limit, and before it bare milliseconds.
 */
async function encodeMaxDelay(w: Writer, version: Version, maxDelay: number | null) {
	if (hasUnlimitedMaxDelay(version)) await Duration.encodeOptional(w, maxDelay ?? undefined);
	else await w.u53(maxDelay ?? MAX_SAFE_DELAY_MS);
}

/** Decode the inverse of {@link encodeMaxDelay}. */
async function decodeMaxDelay(r: Reader, version: Version): Promise<number | null> {
	if (hasUnlimitedMaxDelay(version)) return (await Duration.decodeOptional(r)) ?? null;
	return r.u53();
}

/** Decode the `Subscriber Min Age`, as not stated on a wire without the field. */
async function decodeMinDelay(r: Reader, version: Version): Promise<number | undefined> {
	if (!hasMinDelay(version)) return undefined;
	return Duration.decodeOptional(r);
}

export class SubscribeUpdate {
	/** A refresh request for this group or later, judged against the local track's refresh. */
	wantedGroup?: bigint;
	priority: number;
	/** See {@link Subscribe.order}. */
	order: Order;
	/**
	 * Subscriber max delay in milliseconds, `null` for no limit beyond the publisher's; zero skips
	 * once a newer group is available. Lite-01/02 carry none and decode as `null`.
	 */
	maxDelay: number | null;
	/**
	 * Subscriber min delay (`Subscriber Min Age`) in milliseconds, or undefined when not stated.
	 * Lite-07+; earlier wires decode it as not stated and drop it on encode.
	 */
	minDelay?: number;
	startGroup?: bigint;
	endGroup?: bigint;
	/** See {@link Subscribe.startFrame}. */
	startFrame: number;
	/** See {@link Subscribe.endFrame}. */
	endFrame?: number;
	/**
	 * See {@link Subscribe.ranges}. On lite-07 the wire carries the ranges to add, after retiring
	 * {@link remove}; {@link LiveRanges.apply} resolves a received update to every live range.
	 */
	ranges?: WireRange[];
	/** Lite-07: the Range IDs of the live ranges to retire before adding {@link ranges}. */
	remove?: bigint[];

	constructor(props: {
		priority: number;
		wantedGroup?: bigint;
		order?: Order;
		maxDelay?: number | null;
		minDelay?: number;
		startGroup?: bigint;
		endGroup?: bigint;
		startFrame?: number;
		endFrame?: number;
		ranges?: WireRange[];
		remove?: bigint[];
	}) {
		if (props.wantedGroup !== undefined && props.wantedGroup < 0n)
			throw new RangeError("wanted group must be non-negative");
		this.wantedGroup = props.wantedGroup;
		this.priority = props.priority;
		this.order = props.order ?? "descending";
		this.maxDelay = props.maxDelay === undefined ? 0 : props.maxDelay;
		this.minDelay = props.minDelay;
		this.startGroup = props.startGroup;
		this.endGroup = props.endGroup;
		this.startFrame = props.startFrame ?? 0;
		this.endFrame = props.endFrame;
		this.ranges = props.ranges;
		this.remove = props.remove;
	}

	async #encode(w: Writer, version: Version) {
		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				await w.u8(toWire(this.priority));
				break;
			default:
				await w.u8(toWire(this.priority));
				await encodeOrder(w, version, this.order);
				await encodeMaxDelay(w, version, this.maxDelay);
				await encodeMinDelay(w, version, this.minDelay);
				if (hasRanges(version)) {
					// An edit of the live ranges, which may add none: `ranges`, or else the span as
					// the one range added once `remove` retires the rest.
					if (this.ranges === undefined && this.remove === undefined)
						throw new Error("a lite-07 update edits its ranges");
					const remove = this.remove ?? [];
					await w.u53(remove.length);
					for (const id of remove) await w.u62(id);
					if (this.ranges === undefined) {
						await encodeSpan(w, version, this);
					} else {
						await w.u53(this.ranges.length);
						for (const range of this.ranges) await encodeRange(w, range);
					}
				} else {
					await encodeSpan(w, version, this, this.ranges);
				}
				if (hasRefresh(version)) await w.u62(this.wantedGroup === undefined ? 0n : this.wantedGroup + 1n);
				break;
		}
	}

	static async #decode(r: Reader, version: Version): Promise<SubscribeUpdate> {
		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				return new SubscribeUpdate({ priority: fromWire(await r.u8()), maxDelay: null });
			default: {
				const priority = fromWire(await r.u8());
				const order = await decodeOrder(r, version);
				const maxDelay = await decodeMaxDelay(r, version);
				const minDelay = await decodeMinDelay(r, version);
				let span: Span & { ranges?: WireRange[]; remove?: bigint[] };
				if (hasRanges(version)) {
					// Not preallocated, like the ranges: each id costs a byte of the bounded message.
					const remove: bigint[] = [];
					for (let count = await r.u53(); count > 0; count--) remove.push(await r.u62());
					span = { startFrame: 0, remove, ranges: await decodeRanges(r) };
				} else {
					span = await decodeSpan(r, version);
				}
				const wanted = hasRefresh(version) ? await r.u62() : 0n;
				return new SubscribeUpdate({
					wantedGroup: wanted === 0n ? undefined : wanted - 1n,
					priority,
					order,
					maxDelay,
					minDelay,
					...span,
				});
			}
		}
	}

	async encode(w: Writer, version: Version): Promise<void> {
		if (this.wantedGroup !== undefined && this.wantedGroup < 0n)
			throw new RangeError("wanted group must be non-negative");
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version: Version): Promise<SubscribeUpdate> {
		return Message.decode(r, (r) => SubscribeUpdate.#decode(r, version));
	}

	static async decodeMaybe(r: Reader, version: Version): Promise<SubscribeUpdate | undefined> {
		return Message.decodeMaybe(r, (r) => SubscribeUpdate.#decode(r, version));
	}
}

export class Subscribe {
	id: bigint;
	broadcast: Path.Valid;
	/** The publisher instance the subscriber expects. Lite-07+. */
	instance?: Instance.Valid;
	track: string;
	priority: number;
	/**
	 * Which pending group to send first. Lite-07+; earlier wires decode as descending and
	 * cannot ask for anything else.
	 */
	order: Order;
	/**
	 * Subscriber max delay in milliseconds, `null` for no limit beyond the publisher's; zero skips
	 * once a newer group is available. Lite-01/02 carry none and decode as `null`.
	 */
	maxDelay: number | null;
	/**
	 * Subscriber min delay (`Subscriber Min Age`) in milliseconds, or undefined when not stated.
	 * Lite-07+; earlier wires decode it as not stated and drop it on encode.
	 */
	minDelay?: number;

	startGroup?: bigint;
	endGroup?: bigint;

	/**
	 * First frame to deliver within `startGroup`'s group; 0 is the whole group.
	 * Lite-06+. It qualifies the named group, so it needs `startGroup` to name one
	 * (defined, including 0: group 0 can host a mid-group resume).
	 */
	startFrame: number;

	/**
	 * Last frame to deliver (inclusive) within `endGroup`'s group, or undefined for the
	 * whole group. Lite-06+, and meaningless without an explicit `endGroup`.
	 */
	endFrame?: number;

	/**
	 * Every range. Lite-07 encodes these in place of the span when given, and older versions
	 * the span alone; decoding a lite-07 list fills them alongside the span covering them all.
	 */
	ranges?: WireRange[];

	constructor(props: {
		id: bigint;
		broadcast: Path.Valid;
		instance?: Instance.Valid;
		track: string;
		priority: number;
		order?: Order;
		maxDelay?: number | null;
		minDelay?: number;
		startGroup?: bigint;
		endGroup?: bigint;
		startFrame?: number;
		endFrame?: number;
		ranges?: WireRange[];
	}) {
		this.id = props.id;
		this.broadcast = props.broadcast;
		this.instance = props.instance;
		this.track = props.track;
		this.priority = props.priority;
		this.order = props.order ?? "descending";
		this.maxDelay = props.maxDelay === undefined ? 0 : props.maxDelay;
		this.minDelay = props.minDelay;
		this.startGroup = props.startGroup;
		this.endGroup = props.endGroup;
		this.startFrame = props.startFrame ?? 0;
		this.endFrame = props.endFrame;
		this.ranges = props.ranges;
	}

	async #encode(w: Writer, version: Version) {
		await w.u62(this.id);
		await w.string(Path.encode(this.broadcast));
		await encodePin(w, version, this.instance);
		await encodeNode(w, version);
		await w.string(this.track);
		await w.u8(toWire(this.priority));

		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				break;
			default:
				await encodeOrder(w, version, this.order);
				await encodeMaxDelay(w, version, this.maxDelay);
				await encodeMinDelay(w, version, this.minDelay);
				await encodeSpan(w, version, this, this.ranges);
				break;
		}
	}

	static async #decode(r: Reader, version: Version): Promise<Subscribe> {
		const id = await r.u62();
		const broadcast = Path.decode(await r.string());
		const instance = await decodePin(r, version);
		await decodeNode(r, version);
		const track = await r.string();
		const priority = fromWire(await r.u8());

		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				return new Subscribe({ id, broadcast, track, priority, maxDelay: null });
			default: {
				const order = await decodeOrder(r, version);
				const maxDelay = await decodeMaxDelay(r, version);
				const minDelay = await decodeMinDelay(r, version);
				const span = await decodeSpan(r, version);
				return new Subscribe({
					id,
					broadcast,
					instance,
					track,
					priority,
					order,
					maxDelay,
					minDelay,
					...span,
				});
			}
		}
	}

	async encode(w: Writer, version: Version): Promise<void> {
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version: Version): Promise<Subscribe> {
		return Message.decode(r, (r) => Subscribe.#decode(r, version));
	}
}

/**
 * Publisher's acknowledgement on the Subscribe Stream for drafts 01-04.
 *
 * Draft-05+ replaced this with implicit acceptance plus {@link SubscribeStart} /
 * {@link SubscribeFinish}; the immutable codec/timescale/cache moved to TRACK_INFO.
 */
export class SubscribeOk {
	priority: number;
	/** Accepted subscriber max delay in milliseconds. */
	maxDelay: number;
	startGroup?: bigint;
	endGroup?: bigint;

	constructor({
		priority = 0,
		maxDelay = 0,
		startGroup = undefined,
		endGroup = undefined,
	}: {
		priority?: number;
		maxDelay?: number;
		startGroup?: bigint;
		endGroup?: bigint;
	}) {
		this.priority = priority;
		this.maxDelay = maxDelay;
		this.startGroup = startGroup;
		this.endGroup = endGroup;
	}

	async #encode(w: Writer, version: Version) {
		switch (version) {
			case Version.DRAFT_02:
				// noop
				break;
			case Version.DRAFT_01:
				await w.u8(toWire(this.priority));
				break;
			// Draft-05+ never sends SUBSCRIBE_OK, but keep the field layout matching
			// Draft-03/04 so a stray future use stays well-formed.
			default:
				await w.u8(toWire(this.priority));
				await padGroupOrder(w, version);
				await w.u53(this.maxDelay);
				await w.u62(this.startGroup !== undefined ? this.startGroup + 1n : 0n);
				await w.u62(this.endGroup !== undefined ? this.endGroup + 1n : 0n);
				break;
		}
	}

	static async #decode(version: Version, r: Reader): Promise<SubscribeOk> {
		let priority: number | undefined;
		let maxDelay: number | undefined;
		let startGroup: bigint | undefined;
		let endGroup: bigint | undefined;

		switch (version) {
			case Version.DRAFT_02:
				// noop
				break;
			case Version.DRAFT_01:
				priority = fromWire(await r.u8());
				break;
			default:
				priority = fromWire(await r.u8());
				await skipGroupOrder(r, version);
				maxDelay = await r.u53();
				startGroup = await r.u62();
				endGroup = await r.u62();
				break;
		}

		return new SubscribeOk({
			priority,
			maxDelay,
			startGroup: startGroup !== undefined && startGroup > 0n ? startGroup - 1n : undefined,
			endGroup: endGroup !== undefined && endGroup > 0n ? endGroup - 1n : undefined,
		});
	}

	async encode(w: Writer, version: Version): Promise<void> {
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version: Version): Promise<SubscribeOk> {
		return Message.decode(r, SubscribeOk.#decode.bind(SubscribeOk, version));
	}
}

/** The publisher's largest position, as SUBSCRIBE_OK reports it. Mirrors the Rust `Largest`. */
export interface Largest {
	/** The newest group and its last frame so far. */
	position: Position;
	/** The group ended at that frame, so no frame follows it: a reader knows at once that it isn't the live one. */
	ended: boolean;
}

/**
 * Resolves where a Draft-05+ subscription starts. The first message the publisher sends,
 * once the start is known. A start above the requested one implicitly drops the groups
 * below it.
 *
 * There is no start frame: a partial group is only served to a subscriber that asked for
 * one, so a range begins at its requested frame when it starts at its requested group,
 * and at frame 0 of a later one otherwise.
 */
export class SubscribeStart {
	/**
	 * The group each range's delivery starts at, in turn, `undefined` for a range with no
	 * group there. Before draft-07 the wire carries the one range's start as `Group`.
	 */
	starts: (bigint | undefined)[];

	/**
	 * The publisher's largest when it answered, or `undefined` for a track with nothing yet.
	 * Draft-07+ only; not on the wire before, where it decodes as `undefined`.
	 */
	largest?: Largest;

	constructor(starts: (bigint | undefined)[], largest?: Largest) {
		this.starts = starts;
		this.largest = largest;
	}

	async encode(w: Writer, version: Version): Promise<void> {
		return Message.encode(w, async (w) => {
			// Draft-07 added the largest position along with the ranges.
			if (!hasRanges(version)) {
				const [group, ...rest] = this.starts;
				if (group === undefined || rest.length > 0) throw new Error("one start group before draft-07");
				return w.u62(group);
			}
			// Group + 1, so 0 is a track with nothing yet; the frame and whether the group ended
			// follow only otherwise.
			if (this.largest === undefined) {
				await w.u53(0);
			} else {
				await w.u62(this.largest.position.group + 1n);
				await w.u53(this.largest.position.frame);
				await w.u8(this.largest.ended ? 1 : 0);
			}
			await w.u53(this.starts.length);
			for (const start of this.starts) await w.u62(start === undefined ? 0n : start + 1n);
		});
	}

	static async decode(r: Reader, version: Version): Promise<SubscribeStart> {
		return Message.decode(r, async (r) => {
			if (!hasRanges(version)) return new SubscribeStart([await r.u62()]);
			const wire = await r.u62();
			let largest: Largest | undefined;
			if (wire !== 0n) {
				const position = { group: wire - 1n, frame: await r.u53() };
				const ended = await r.u8();
				if (ended > 1) throw new ProtocolViolation(`invalid Largest Ended: ${ended}`);
				largest = { position, ended: ended === 1 };
			}
			// Not preallocated: each start costs a byte of the bounded message.
			const starts: (bigint | undefined)[] = [];
			for (let count = await r.u53(); count > 0; count--) {
				const start = await r.u62();
				starts.push(start === 0n ? undefined : start - 1n);
			}
			return new SubscribeStart(starts, largest);
		});
	}
}

/**
 * SUBSCRIBE_FINISH (SUBSCRIBE_END before lite-07): no group at or after `group` (exclusive
 * upper bound) will be produced on a Draft-05+ subscription. `0` means the track ended before
 * producing any groups.
 */
export class SubscribeFinish {
	/** The exclusive final group key: the first key that will never be produced. */
	group: bigint;
	/** The stream error code the track ended with, if it did not end cleanly. Draft-07+ only. */
	error?: number;

	constructor(group: bigint, error?: number) {
		this.group = group;
		this.error = error;
	}

	/** The error the track ended with, or `undefined` for a clean end. */
	cause(): Error | undefined {
		if (this.error === undefined) return undefined;
		if (this.error > 0xffff_ffff)
			return new StreamError(StreamCode.Internal, { message: `invalid end code: ${this.error}` });
		return fromTransport({ source: "stream", streamErrorCode: this.error });
	}

	async encode(w: Writer, version: Version): Promise<void> {
		return Message.encode(w, async (w) => {
			await w.u62(this.group);
			// The code + 1, so 0 is a clean end.
			if (hasEndError(version)) await w.u53(this.error === undefined ? 0 : this.error + 1);
			else if (this.error !== undefined) throw new Error("an errored end needs draft-07");
		});
	}

	static async decode(r: Reader, version: Version): Promise<SubscribeFinish> {
		return Message.decode(r, async (r) => {
			const group = await r.u62();
			if (!hasEndError(version)) return new SubscribeFinish(group);
			const code = await r.u53();
			return new SubscribeFinish(group, code === 0 ? undefined : code - 1);
		});
	}
}

/** SUBSCRIBE_STARTED: where a range a SUBSCRIBE_UPDATE added starts. Lite-07+. */
export class SubscribeStarted {
	/** The range's Range ID. */
	id: bigint;
	/** The group its delivery starts at, `undefined` for a range with no group there. */
	start?: bigint;

	constructor(id: bigint, start?: bigint) {
		this.id = id;
		this.start = start;
	}

	async encode(w: Writer): Promise<void> {
		return Message.encode(w, async (w) => {
			await w.u62(this.id);
			await w.u62(this.start === undefined ? 0n : this.start + 1n);
		});
	}

	static async decode(r: Reader): Promise<SubscribeStarted> {
		return Message.decode(r, async (r) => {
			const id = await r.u62();
			const start = await r.u62();
			return new SubscribeStarted(id, start === 0n ? undefined : start - 1n);
		});
	}
}

/**
 * A SUBSCRIBE_DROP or SUBSCRIBE_SKIP range: a non-empty `[Group Start, Group End)` on lite-07, an
 * inclusive `Group Start` and `Group End` before. Either way it ends by the reserved key
 * 2^64-1, so it never names that group.
 */
async function decodeGroups(r: Reader, version: Version): Promise<{ start: bigint; end: bigint }> {
	const start = await r.u62();
	const span = hasGroupSpan(version);
	const end = span ? await r.u62() : (await r.u62()) + 1n;
	if (end < start || (span && end === start) || end >= UNBOUNDED)
		throw new ProtocolViolation(`invalid group range: [${start}, ${end})`);
	return { start, end };
}

async function encodeGroups(w: Writer, start: bigint, end: bigint, version: Version) {
	const span = hasGroupSpan(version);
	// An empty range at 0 has no inclusive end to spell before lite-07, and lite-07 has no empty one.
	if (end < start || end >= UNBOUNDED || (span ? end === start : end === 0n))
		throw new RangeError(`invalid group range: [${start}, ${end})`);
	await w.u62(start);
	await w.u62(span ? end : end - 1n);
}

/**
 * Names groups that existed but will not be delivered: aborted or reset, evicted or expired with
 * no copy left, or stale.
 *
 * Draft-03+; publishers send it on Draft-05 and later. Before lite-07 it also names keys
 * declared skipped, which lite-07 names in {@link SubscribeSkip}.
 */
export class SubscribeDrop {
	/** The first dropped group key. */
	start: bigint;
	/** One past the last dropped group key. */
	end: bigint;
	error: number;

	constructor(props: { start: bigint; end: bigint; error: number }) {
		this.start = props.start;
		this.end = props.end;
		this.error = props.error;
	}

	/** Name `[start, end)` given up with `err`, as `version` carries it. */
	static given(start: bigint, end: bigint, err: unknown, version: Version): SubscribeDrop {
		return new SubscribeDrop({
			start,
			end,
			// Before lite-07 the code is the application's, so it says nothing of why.
			error: hasDropCode(version) ? toStreamCode(err) : 0,
		});
	}

	/**
	 * The error the dropped groups' readers end with, as `version` reads the code.
	 *
	 * Before lite-07 the code is the application's, so every drop reads as OLD: the publisher
	 * gave the groups up, and a reader moves on to a newer one as it would past a superseded group.
	 */
	cause(version: Version): Error {
		if (!hasDropCode(version)) return new StreamError(StreamCode.Old);
		if (this.error > 0xffff_ffff)
			return new StreamError(StreamCode.Internal, { message: `invalid drop code: ${this.error}` });
		return fromTransport({ source: "stream", streamErrorCode: this.error });
	}

	async #encode(w: Writer, version: Version) {
		await encodeGroups(w, this.start, this.end, version);
		await w.u53(this.error);
	}

	static async #decode(r: Reader, version: Version): Promise<SubscribeDrop> {
		const { start, end } = await decodeGroups(r, version);
		const error = await r.u53();
		return new SubscribeDrop({ start, end, error });
	}

	async encode(w: Writer, version: Version): Promise<void> {
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version: Version): Promise<SubscribeDrop> {
		return Message.decode(r, (r) => SubscribeDrop.#decode(r, version));
	}
}

/**
 * Names keys the publisher declared it will never produce (see `Producer.skipGroups`):
 * final, with no error, ending no reader. Lite-07+; earlier versions name them in a
 * {@link SubscribeDrop}.
 */
export class SubscribeSkip {
	/** The first skipped group key. */
	start: bigint;
	/** One past the last skipped group key. */
	end: bigint;

	constructor(props: { start: bigint; end: bigint }) {
		this.start = props.start;
		this.end = props.end;
	}

	async encode(w: Writer, version: Version): Promise<void> {
		return Message.encode(w, (w) => encodeGroups(w, this.start, this.end, version));
	}

	static async decode(r: Reader, version: Version): Promise<SubscribeSkip> {
		return Message.decode(r, async (r) => new SubscribeSkip(await decodeGroups(r, version)));
	}
}

/**
 * A response message on the subscribe stream, prefixed with a type discriminator
 * on Draft-03+.
 *
 * The discriminator is version-dependent:
 * - Draft-03/04: `0x0` SUBSCRIBE_OK, `0x1` SUBSCRIBE_DROP.
 * - Draft-05+: `0x0` SUBSCRIBE_START (SUBSCRIBE_OK on lite-07), `0x1` SUBSCRIBE_FINISH
 *   (SUBSCRIBE_END before lite-07), `0x2` SUBSCRIBE_DROP (SUBSCRIBE_OK was removed;
 *   acceptance is implicit).
 * - Lite-07+: also `0x3` SUBSCRIBE_SKIP and `0x4` SUBSCRIBE_STARTED.
 */
export type SubscribeResponse =
	| { ok: SubscribeOk }
	| { start: SubscribeStart }
	| { finish: SubscribeFinish }
	| { drop: SubscribeDrop }
	| { skip: SubscribeSkip }
	| { started: SubscribeStarted };

export async function encodeSubscribeResponse(w: Writer, resp: SubscribeResponse, version: Version): Promise<void> {
	switch (version) {
		case Version.DRAFT_01:
		case Version.DRAFT_02:
			if ("ok" in resp) {
				await resp.ok.encode(w, version);
			} else {
				throw new Error("only SUBSCRIBE_OK is supported for this version");
			}
			break;
		case Version.DRAFT_03:
		case Version.DRAFT_04:
			if ("ok" in resp) {
				await w.u53(0x0);
				await resp.ok.encode(w, version);
			} else if ("drop" in resp) {
				await w.u53(0x1);
				await resp.drop.encode(w, version);
			} else {
				throw new Error("SUBSCRIBE_START/FINISH not supported for this version");
			}
			break;
		default:
			// Draft-05+: SUBSCRIBE_OK is gone; START/END/DROP carry the resolved range.
			if ("start" in resp) {
				await w.u53(0x0);
				await resp.start.encode(w, version);
			} else if ("finish" in resp) {
				await w.u53(0x1);
				await resp.finish.encode(w, version);
			} else if ("drop" in resp) {
				await w.u53(0x2);
				await resp.drop.encode(w, version);
			} else if ("skip" in resp && hasSkip(version)) {
				await w.u53(0x3);
				await resp.skip.encode(w, version);
			} else if ("started" in resp && hasRanges(version)) {
				await w.u53(0x4);
				await resp.started.encode(w);
			} else {
				throw new Error("subscribe response not supported for this version");
			}
			break;
	}
}

export async function decodeSubscribeResponse(r: Reader, version: Version): Promise<SubscribeResponse> {
	switch (version) {
		case Version.DRAFT_01:
		case Version.DRAFT_02:
			return { ok: await SubscribeOk.decode(r, version) };
		case Version.DRAFT_03:
		case Version.DRAFT_04: {
			const typ = await r.u53();
			switch (typ) {
				case 0x0:
					return { ok: await SubscribeOk.decode(r, version) };
				case 0x1:
					return { drop: await SubscribeDrop.decode(r, version) };
				default:
					throw new ProtocolViolation(`unknown subscribe response type: ${typ}`);
			}
		}
		default: {
			const typ = await r.u53();
			switch (typ) {
				case 0x0:
					return { start: await SubscribeStart.decode(r, version) };
				case 0x1:
					return { finish: await SubscribeFinish.decode(r, version) };
				case 0x2:
					return { drop: await SubscribeDrop.decode(r, version) };
				case 0x3:
					if (!hasSkip(version)) throw new ProtocolViolation(`unknown subscribe response type: ${typ}`);
					return { skip: await SubscribeSkip.decode(r, version) };
				case 0x4:
					if (!hasRanges(version)) throw new ProtocolViolation(`unknown subscribe response type: ${typ}`);
					return { started: await SubscribeStarted.decode(r) };
				default:
					throw new ProtocolViolation(`unknown subscribe response type: ${typ}`);
			}
		}
	}
}

/** Like {@link decodeSubscribeResponse} but resolves `undefined` on a clean FIN. */
export async function decodeSubscribeResponseMaybe(
	r: Reader,
	version: Version,
): Promise<SubscribeResponse | undefined> {
	if (await r.done()) return undefined;
	return decodeSubscribeResponse(r, version);
}

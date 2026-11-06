import {
	fromTransport,
	ProtocolViolation,
	StreamCode,
	StreamError,
	TimestampMismatch,
	toStreamCode,
} from "../error.ts";
import type * as Instance from "../instance.ts";
import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import { floorAt, type Timescale, Timestamp } from "../time.ts";
import type { Anchor, Order, Position, Range } from "../track.ts";
import { lengthLeadingOnes, parts, split } from "../util/varint.ts";
import * as Duration from "./duration.ts";
import { decodePin, encodePin } from "./instance.ts";
import * as Message from "./message.ts";
import { fromWire, toWire } from "./priority.ts";
import {
	hasDropCode,
	hasEndError,
	hasFrameBounds,
	hasGroupOrder,
	hasMinDelay,
	hasOrder,
	hasRanges,
	hasRefresh,
	resolvesStart,
	Version,
} from "./version.ts";

/**
 * Encode the `Group Start` field shared by SUBSCRIBE and SUBSCRIBE_UPDATE.
 *
 * Lite-06 writes the raw floor (`undefined` and 0 are both group 0). A pre-06 wire
 * encodes the sequence + 1: omitting the floor is 0 (the latest group, where the
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
 * One end of a lite-07 range as the wire carries it: a model {@link Anchor}, except that a time
 * counts in the track's `Timescale` as the peer was told it.
 */
export type WireAnchor =
	| { absolute: bigint; relative?: never; time?: never }
	| { relative: bigint; absolute?: never; time?: never }
	| { time: number; absolute?: never; relative?: never };

/** A lite-07 range as the wire carries it; see {@link WireAnchor}. */
export interface WireRange {
	start?: WireAnchor;
	end?: WireAnchor;
}

/**
 * The wire form of `ranges`, with time anchors in `timescale`, the track's as the publisher
 * declared it. Throws {@link TimestampMismatch} for a time anchor on an untimed track.
 */
export function toWireRanges(ranges: readonly Range[], timescale: Timescale | undefined): WireRange[] {
	const anchor = (anchor: Anchor | undefined): WireAnchor | undefined => {
		if (anchor?.time === undefined) return anchor as WireAnchor | undefined;
		if (timescale === undefined) throw new TimestampMismatch();
		return { time: floorAt(anchor.time, timescale) };
	};
	return ranges.map((range) => {
		const out: WireRange = {};
		const start = anchor(range.start);
		const end = anchor(range.end);
		if (start) out.start = start;
		if (end) out.end = end;
		return out;
	});
}

/** Whether any range starts or ends at a presentation time, which needs the track's timescale. */
export function hasWireTime(ranges: readonly WireRange[] | undefined): boolean {
	return ranges?.some((range) => range.start?.time !== undefined || range.end?.time !== undefined) ?? false;
}

/** Encode one lite-07 range end, `undefined` for none: an absolute end names a group's first frame. */
async function encodeAnchor(w: Writer, anchor: WireAnchor | undefined) {
	if (anchor?.absolute !== undefined) {
		await w.u53(1);
		await w.u62(anchor.absolute);
		await w.u53(0);
	} else if (anchor?.relative !== undefined) {
		await w.u53(2);
		await w.u62(anchor.relative);
	} else if (anchor?.time !== undefined) {
		await w.u53(3);
		await w.u53(anchor.time);
	} else {
		await w.u53(0);
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
		for (const range of ranges) {
			await encodeAnchor(w, range.start);
			await encodeAnchor(w, range.end);
		}
		return;
	}
	if (!hasRanges(version)) {
		await encodeStartGroup(w, version, span.startGroup);
		await w.u62(span.endGroup !== undefined ? span.endGroup + 1n : 0n);
		await encodeFrameBounds(w, version, span);
		return;
	}

	const { startGroup, startFrame, endGroup, endFrame } = span;
	if ((startFrame !== 0 && startGroup === undefined) || (endFrame !== undefined && endGroup === undefined)) {
		throw new Error("frame bound without a group bound");
	}
	await w.u53(1);
	// Each end is a type, then its fields: 0 is none (the oldest group as a start, the live
	// edge as an end) and 1 an absolute position, which an end stops just before.
	if (startGroup === undefined) {
		await w.u53(0);
	} else {
		await w.u53(1);
		await w.u62(startGroup);
		await w.u53(startFrame);
	}
	if (endGroup === undefined) {
		await w.u53(0);
	} else {
		await w.u53(1);
		await w.u62(endFrame === undefined ? endGroup + 1n : endGroup);
		await w.u53(endFrame === undefined ? 0 : endFrame + 1);
	}
}

/**
 * The model ranges a decoded SUBSCRIBE or SUBSCRIBE_UPDATE asks for: its lite-07 list, or the one
 * span an older wire carries, whole groups either way. A time anchor counts in `timescale`, the
 * track's as the peer was told it, and is refused with {@link TimestampMismatch} without one.
 */
export function requestedRanges(msg: Span & { ranges?: WireRange[] }, timescale: Timescale | undefined): Range[] {
	if (msg.ranges) {
		const anchor = (anchor: WireAnchor | undefined): Anchor | undefined => {
			if (anchor?.time === undefined) return anchor as Anchor | undefined;
			if (timescale === undefined) throw new TimestampMismatch();
			return { time: new Timestamp(anchor.time, timescale) };
		};
		return msg.ranges.map((range) => {
			const out: Range = {};
			const start = anchor(range.start);
			const end = anchor(range.end);
			if (start) out.start = start;
			if (end) out.end = end;
			return out;
		});
	}
	const range: Range = {};
	if (msg.startGroup !== undefined) range.start = { absolute: msg.startGroup };
	if (msg.endGroup !== undefined) range.end = { absolute: msg.endGroup + 1n };
	return [range];
}

/** A position as a comparable pair, group then frame. */
export type Point = [group: bigint, frame: number];

function before(a: Point, b: Point): boolean {
	return a[0] < b[0] || (a[0] === b[0] && a[1] < b[1]);
}

/**
 * Decode one lite-07 range end: its absolute position, if any, and the wire anchor, `undefined`
 * for none. An absolute end past frame 0 includes its group.
 */
async function decodeAnchor(r: Reader, end: boolean): Promise<{ point?: Point; anchor?: WireAnchor }> {
	const kind = await r.u53();
	switch (kind) {
		case 0:
			return {};
		case 1: {
			const point: Point = [await r.u62(), await r.u53()];
			return { point, anchor: { absolute: end && point[1] > 0 ? point[0] + 1n : point[0] } };
		}
		case 2:
			return { anchor: { relative: await r.u62() } };
		case 3:
			return { anchor: { time: await r.u53() } };
		default:
			throw new ProtocolViolation(`unknown range anchor: ${kind}`);
	}
}

/**
 * A decoded lite-07 range, with the positions its absolute anchors name, which a
 * {@link WireRange} rounds to whole groups.
 */
export interface RangeEntry {
	range: WireRange;
	from?: Point;
	to?: Point;
}

/** Decode one lite-07 range: its start, then its end. */
async function decodeEntry(r: Reader): Promise<RangeEntry> {
	const from = await decodeAnchor(r, false);
	const to = await decodeAnchor(r, true);
	const range: WireRange = {};
	if (from.anchor) range.start = from.anchor;
	if (to.anchor) range.end = to.anchor;
	// Empty partway through a group, which whole groups can't say: keep its place,
	// covering no group, as the Rust publisher does.
	if (from.point && to.point && !before(from.point, to.point)) range.end = { absolute: from.point[0] };
	return { range, from: from.point, to: to.point };
}

/** Decode a lite-07 `Range Count` and its ranges, which may be none. */
async function decodeEntries(r: Reader): Promise<RangeEntry[]> {
	// Not preallocated: each range costs at least two bytes of the bounded message.
	const entries: RangeEntry[] = [];
	for (let count = await r.u53(); count > 0; count--) entries.push(await decodeEntry(r));
	return entries;
}

/**
 * Lite-07 `entries` as their ranges and the one span covering every one of them, which the
 * subscriber narrows again.
 */
export function spanOf(version: Version, entries: readonly RangeEntry[]): Span & { ranges: WireRange[] } {
	let start: Point | undefined;
	let end: Point | undefined;
	let floor = true;
	let live = false;
	for (const { from, to } of entries) {
		if (from === undefined) floor = false;
		else if (start === undefined || before(from, start)) start = from;
		if (to === undefined) live = true;
		else if (end === undefined || before(end, to)) end = to;
	}
	if (!floor) start = undefined;
	if (live) end = undefined;
	// An exclusive end at the very first position names nothing.
	if (end !== undefined && end[0] === 0n && end[1] === 0) throw new ProtocolViolation(EMPTY_RANGE);

	const startFrame = start?.[1] ?? 0;
	return {
		ranges: entries.map((entry) => entry.range),
		startGroup: canonicalStartGroup(version, start?.[0], startFrame),
		startFrame,
		endGroup: end === undefined ? undefined : end[1] === 0 ? end[0] - 1n : end[0],
		endFrame: end === undefined || end[1] === 0 ? undefined : end[1] - 1,
	};
}

/**
 * The inverse of {@link encodeSpan}. A lite-07 list decodes as its ranges and as the one span
 * covering every range in it, which the subscriber narrows again.
 */
async function decodeSpan(
	r: Reader,
	version: Version,
): Promise<Span & { ranges?: WireRange[]; entries?: RangeEntry[] }> {
	if (!hasRanges(version)) {
		const startGroup = await decodeStartGroup(r, version);
		const endGroup = (await r.u62()) || undefined;
		const end = endGroup !== undefined ? endGroup - 1n : undefined;
		const frames = await decodeFrameBounds(r, version, startGroup, end);
		return { startGroup: canonicalStartGroup(version, startGroup, frames.startFrame), endGroup: end, ...frames };
	}

	const entries = await decodeEntries(r);
	if (entries.length === 0) throw new ProtocolViolation("a subscription asks for no range");
	return { ...spanOf(version, entries), entries };
}

/**
 * The ranges `msg` carries as lite-07 entries: its decoded list, or else the one span it
 * encodes as its only range.
 */
export function entriesOf(msg: Span & { ranges?: WireRange[]; entries?: RangeEntry[] }): RangeEntry[] {
	if (msg.entries) return msg.entries;
	if (msg.ranges) return msg.ranges.map((range) => ({ range }));
	const range: WireRange = {};
	const entry: RangeEntry = { range };
	if (msg.startGroup !== undefined) {
		range.start = { absolute: msg.startGroup };
		entry.from = [msg.startGroup, msg.startFrame];
	}
	if (msg.endGroup !== undefined) {
		entry.to = msg.endFrame === undefined ? [msg.endGroup + 1n, 0] : [msg.endGroup, msg.endFrame + 1];
		range.end = { absolute: msg.endGroup + 1n };
	}
	return [entry];
}

/** The bytes a lite-07 range takes on the wire, whose varints count leading ones. */
function entrySize({ range, from, to }: RangeEntry): number {
	const size = (value: number | bigint) => {
		const lo = split(value);
		return lengthLeadingOnes(parts.hi, lo);
	};
	const anchor = (anchor: WireAnchor | undefined, point: Point | undefined): number => {
		if (point) return size(1) + size(point[0]) + size(point[1]);
		if (anchor?.absolute !== undefined) return size(1) + size(anchor.absolute) + size(0);
		if (anchor?.relative !== undefined) return size(2) + size(anchor.relative);
		if (anchor?.time !== undefined) return size(3) + size(anchor.time);
		return size(0);
	};
	return anchor(range.start, from) + anchor(range.end, to);
}

/** A range's anchors as a key, so equal ranges compare equal. */
function rangeKey(range: WireRange): string {
	const anchor = (anchor: WireAnchor | undefined) => {
		if (anchor?.absolute !== undefined) return `a${anchor.absolute}`;
		if (anchor?.relative !== undefined) return `r${anchor.relative}`;
		if (anchor?.time !== undefined) return `t${anchor.time}`;
		return "-";
	};
	return `${anchor(range.start)}:${anchor(range.end)}`;
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
	readonly #live = new Map<bigint, RangeEntry>();
	// The bytes the live ranges take on the wire, held to what one SUBSCRIBE carries.
	#size = 0;
	/** How many ranges the SUBSCRIBE carried, whose starts SUBSCRIBE_START reports. */
	readonly initial: bigint;

	/** The ranges a SUBSCRIBE carried. */
	constructor(version: Version, ranges: readonly (RangeEntry | WireRange)[]) {
		this.#version = version;
		this.initial = BigInt(ranges.length);
		this.#add(ranges);
	}

	#add(ranges: readonly (RangeEntry | WireRange)[]) {
		for (const range of ranges) {
			const entry = "range" in range ? (range as RangeEntry) : { range: range as WireRange };
			// Only lite-07 numbers ranges on the wire, and only it is held to one SUBSCRIBE.
			if (hasRanges(this.#version)) this.#size += entrySize(entry);
			this.#live.set(this.#next++, entry);
		}
	}

	#remove(id: bigint): boolean {
		const entry = this.#live.get(id);
		if (!entry) return false;
		if (hasRanges(this.#version)) this.#size -= entrySize(entry);
		this.#live.delete(id);
		return true;
	}

	/** The live range with Range ID `id`. */
	get(id: bigint): WireRange | undefined {
		return this.#live.get(id)?.range;
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
		return [...this.#live.values()].map((entry) => entry.range);
	}

	/** Every live range with its Range ID, in order. */
	entries(): [bigint, WireRange][] {
		return [...this.#live].map(([id, entry]) => [id, entry.range]);
	}

	/** Whether the publisher reports where each range starts, which lite-07 does by Range ID. */
	get reportsStarts(): boolean {
		return hasRanges(this.#version);
	}

	/**
	 * Apply a received lite-07 `update`'s edit, returning every live range by Range ID. Throws
	 * {@link ProtocolViolation} for an edit it cannot apply. Before lite-07 an update restates
	 * its ranges, so nothing is numbered.
	 */
	apply(update: SubscribeUpdate): { id: bigint; entry: RangeEntry }[] {
		if (!hasRanges(this.#version)) return [];
		for (const id of update.remove ?? []) {
			// An id never assigned, or already retired (including earlier in this list).
			if (!this.#remove(id)) throw new ProtocolViolation(`unknown range id: ${id}`);
		}
		this.#add(update.entries ?? update.ranges ?? []);
		// Asking for nothing is refused like a SUBSCRIBE that does, and a set no SUBSCRIBE
		// could carry like one too large to send.
		if (this.#live.size === 0) throw new ProtocolViolation("a subscription asks for no range");
		if (this.#size > Message.MAX_MESSAGE_SIZE) throw new ProtocolViolation("too many ranges");
		return [...this.#live].map(([id, entry]) => ({ id, entry }));
	}

	/**
	 * The edit that turns the live ranges into `ranges`, which become the live ones: the Range IDs
	 * to retire and the ranges to add. A range already live keeps its Range ID, so an update that
	 * changes one range sends only that one, and the publisher keeps where the rest resolved.
	 * Before lite-07 replaced ranges take fresh Range IDs.
	 */
	edit(ranges: readonly WireRange[]): { remove: bigint[]; add: WireRange[] } {
		if (ranges.length === 0) throw new Error(EMPTY_RANGE);
		if (!hasRanges(this.#version)) {
			const same =
				ranges.length === this.#live.size &&
				this.ranges().every((range, index) => rangeKey(range) === rangeKey(ranges[index]));
			if (!same) {
				this.#live.clear();
				this.#add(ranges);
			}
			return { remove: [], add: [...ranges] };
		}
		// Each live range keeps at most one wanted copy, so duplicates edit like any other.
		const wanted = new Map<string, number>();
		for (const range of ranges) wanted.set(rangeKey(range), (wanted.get(rangeKey(range)) ?? 0) + 1);
		const remove: bigint[] = [];
		for (const [id, entry] of this.#live) {
			const key = rangeKey(entry.range);
			const count = wanted.get(key) ?? 0;
			if (count > 0) wanted.set(key, count - 1);
			else remove.push(id);
		}
		// In the order asked for, so the publisher numbers them the way this side does.
		const add = ranges.filter((range) => {
			const key = rangeKey(range);
			const count = wanted.get(key) ?? 0;
			if (count === 0) return false;
			wanted.set(key, count - 1);
			return true;
		});
		for (const id of remove) this.#remove(id);
		this.#add(add);
		if (this.#size > Message.MAX_MESSAGE_SIZE) throw new Error("too many ranges for one SUBSCRIBE");
		return { remove, add };
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

/** Decode the `Subscriber Min Age`, as not stated on a wire without the field. */
async function decodeMinDelay(r: Reader, version: Version): Promise<number | undefined> {
	if (!hasMinDelay(version)) return undefined;
	return Duration.decodeOptional(r);
}

export class SubscribeUpdate {
	/** A join requesting this group or later, judged against the local refresh ratio. */
	wantedGroup?: bigint;
	priority: number;
	/** See {@link Subscribe.order}. */
	order: Order;
	/** Subscriber max delay in milliseconds; zero skips once a newer group is available. */
	maxDelay: number;
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
	/** Lite-07: the ranges a received update adds, with the positions they name; see {@link RangeEntry}. */
	entries?: RangeEntry[];

	constructor(props: {
		priority: number;
		wantedGroup?: bigint;
		order?: Order;
		maxDelay?: number;
		minDelay?: number;
		startGroup?: bigint;
		endGroup?: bigint;
		startFrame?: number;
		endFrame?: number;
		ranges?: WireRange[];
		remove?: bigint[];
		entries?: RangeEntry[];
	}) {
		if (props.wantedGroup !== undefined && props.wantedGroup < 0n)
			throw new RangeError("wanted group must be non-negative");
		this.wantedGroup = props.wantedGroup;
		this.priority = props.priority;
		this.order = props.order ?? "descending";
		this.maxDelay = props.maxDelay ?? 0;
		this.minDelay = props.minDelay;
		this.startGroup = props.startGroup;
		this.endGroup = props.endGroup;
		this.startFrame = props.startFrame ?? 0;
		this.endFrame = props.endFrame;
		this.ranges = props.ranges;
		this.remove = props.remove;
		this.entries = props.entries;
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
				await w.u53(this.maxDelay);
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
						for (const range of this.ranges) {
							await encodeAnchor(w, range.start);
							await encodeAnchor(w, range.end);
						}
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
				return new SubscribeUpdate({ priority: fromWire(await r.u8()) });
			default: {
				const priority = fromWire(await r.u8());
				const order = await decodeOrder(r, version);
				const maxDelay = await r.u53();
				const minDelay = await decodeMinDelay(r, version);
				let span: Span & { ranges?: WireRange[]; remove?: bigint[]; entries?: RangeEntry[] };
				if (hasRanges(version)) {
					// Not preallocated, like the ranges: each id costs a byte of the bounded message.
					const remove: bigint[] = [];
					for (let count = await r.u53(); count > 0; count--) remove.push(await r.u62());
					const entries = await decodeEntries(r);
					span = { startFrame: 0, remove, entries, ranges: entries.map((entry) => entry.range) };
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
	/** Subscriber max delay in milliseconds; zero skips once a newer group is available. */
	maxDelay: number;
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
	/** Lite-07: the decoded ranges with the positions they name; see {@link RangeEntry}. */
	entries?: RangeEntry[];

	constructor(props: {
		id: bigint;
		broadcast: Path.Valid;
		instance?: Instance.Valid;
		track: string;
		priority: number;
		order?: Order;
		maxDelay?: number;
		minDelay?: number;
		startGroup?: bigint;
		endGroup?: bigint;
		startFrame?: number;
		endFrame?: number;
		ranges?: WireRange[];
		entries?: RangeEntry[];
	}) {
		this.id = props.id;
		this.broadcast = props.broadcast;
		this.instance = props.instance;
		this.track = props.track;
		this.priority = props.priority;
		this.order = props.order ?? "descending";
		this.maxDelay = props.maxDelay ?? 0;
		this.minDelay = props.minDelay;
		this.startGroup = props.startGroup;
		this.endGroup = props.endGroup;
		this.startFrame = props.startFrame ?? 0;
		this.endFrame = props.endFrame;
		this.ranges = props.ranges;
		this.entries = props.entries;
	}

	async #encode(w: Writer, version: Version) {
		await w.u62(this.id);
		await w.string(Path.encode(this.broadcast));
		await encodePin(w, version, this.instance);
		await w.string(this.track);
		await w.u8(toWire(this.priority));

		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				break;
			default:
				await encodeOrder(w, version, this.order);
				await w.u53(this.maxDelay);
				await encodeMinDelay(w, version, this.minDelay);
				await encodeSpan(w, version, this, this.ranges);
				break;
		}
	}

	static async #decode(r: Reader, version: Version): Promise<Subscribe> {
		const id = await r.u62();
		const broadcast = Path.decode(await r.string());
		const instance = await decodePin(r, version);
		const track = await r.string();
		const priority = fromWire(await r.u8());

		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				return new Subscribe({ id, broadcast, track, priority });
			default: {
				const order = await decodeOrder(r, version);
				const maxDelay = await r.u53();
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
	 * The publisher's largest (group, frame) when it answered, or `undefined` for a track
	 * with nothing yet. Draft-07+ only; not on the wire before, where it decodes as `undefined`.
	 */
	largest?: Position;

	constructor(starts: (bigint | undefined)[], largest?: Position) {
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
			// Group + 1, so 0 is a track with nothing yet; the frame follows only otherwise.
			if (this.largest === undefined) {
				await w.u53(0);
			} else {
				await w.u62(this.largest.group + 1n);
				await w.u53(this.largest.frame);
			}
			await w.u53(this.starts.length);
			for (const start of this.starts) await w.u62(start === undefined ? 0n : start + 1n);
		});
	}

	static async decode(r: Reader, version: Version): Promise<SubscribeStart> {
		return Message.decode(r, async (r) => {
			if (!hasRanges(version)) return new SubscribeStart([await r.u62()]);
			const wire = await r.u62();
			const largest = wire === 0n ? undefined : { group: wire - 1n, frame: await r.u53() };
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
	/** The exclusive final group sequence: the first sequence that will never be produced. */
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

/// Indicates that one or more groups have been dropped.
///
/// Draft-03+; publishers send it on Draft-05 and later.
export class SubscribeDrop {
	start: bigint;
	end: bigint;
	error: number;

	constructor(props: { start: bigint; end: bigint; error: number }) {
		this.start = props.start;
		this.end = props.end;
		this.error = props.error;
	}

	/** Name `[start, end]` given up with `err`, as `version` carries it. */
	static given(start: bigint, end: bigint, err: unknown, version: Version): SubscribeDrop {
		// Before lite-07 the code is the application's, so it says nothing of why.
		return new SubscribeDrop({ start, end, error: hasDropCode(version) ? toStreamCode(err) : 0 });
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

	async #encode(w: Writer) {
		if (this.start - this.end > 1n) throw new RangeError("invalid dropped range");
		await w.u62(this.start);
		await w.u62(this.end);
		await w.u53(this.error);
	}

	static async #decode(r: Reader): Promise<SubscribeDrop> {
		const drop = new SubscribeDrop({ start: await r.u62(), end: await r.u62(), error: await r.u53() });
		if (drop.start - drop.end > 1n) throw new ProtocolViolation("invalid dropped range");
		return drop;
	}

	async encode(w: Writer): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader): Promise<SubscribeDrop> {
		return Message.decode(r, SubscribeDrop.#decode);
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
 * - Lite-07+: also `0x4` SUBSCRIBE_STARTED.
 */
export type SubscribeResponse =
	| { ok: SubscribeOk }
	| { start: SubscribeStart }
	| { finish: SubscribeFinish }
	| { drop: SubscribeDrop }
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
				await resp.drop.encode(w);
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
				await resp.drop.encode(w);
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
					return { drop: await SubscribeDrop.decode(r) };
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
					return { drop: await SubscribeDrop.decode(r) };
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

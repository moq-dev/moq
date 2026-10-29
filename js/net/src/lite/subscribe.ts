import type * as Epoch from "../epoch.ts";
import {
	fromTransport,
	ProtocolViolation,
	StreamCode,
	StreamError,
	TimestampMismatch,
	toStreamCode,
} from "../error.ts";
import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import { type Timescale, Timestamp } from "../time.ts";
import type { Anchor, Order, Position, Range } from "../track.ts";
import { decodeEpoch, encodeEpoch } from "./epoch.ts";
import * as Message from "./message.ts";
import { fromWire, toWire } from "./priority.ts";
import {
	hasDropCode,
	hasFrameBounds,
	hasGroupOrder,
	hasLargest,
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
async function encodeStartGroup(w: Writer, version: Version, startGroup?: number) {
	if (resolvesStart(version)) {
		await w.u53(startGroup ?? 0);
		return;
	}
	await w.u53(startGroup === undefined ? 0 : startGroup + 1);
}

/**
 * Decode the `Group Start` field shared by SUBSCRIBE and SUBSCRIBE_UPDATE.
 *
 * The inverse of {@link encodeStartGroup}. Callers canonicalize with
 * {@link canonicalStartGroup} once the frame bounds are known.
 */
async function decodeStartGroup(r: Reader, version: Version): Promise<number | undefined> {
	const value = await r.u53();
	if (resolvesStart(version)) return value;
	return value > 0 ? value - 1 : undefined;
}

/**
 * Canonicalize a decoded floor: a lite-06 `Group Start` of 0 with no frame offset is the
 * same absence of a constraint as no floor at all, so it decodes as undefined. Group 0
 * stays named only when a `Frame Start` actually qualifies it (a subscription can resume
 * partway through group 0: a catalog never leaves it).
 */
function canonicalStartGroup(version: Version, startGroup: number | undefined, startFrame: number): number | undefined {
	if (resolvesStart(version) && startGroup === 0 && startFrame === 0) return undefined;
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
	}: { startGroup?: number; startFrame: number; endGroup?: number; endFrame?: number },
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
	startGroup?: number,
	endGroup?: number,
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
	startGroup?: number;
	startFrame: number;
	endGroup?: number;
	endFrame?: number;
}

/**
 * One end of a lite-07 range as the wire carries it: a model {@link Anchor}, except that a time
 * counts in the track's `Timescale` as the peer was told it.
 */
export type WireAnchor =
	| { absolute: number; relative?: never; time?: never }
	| { relative: number; absolute?: never; time?: never }
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
		return { time: Math.floor(anchor.time.as(timescale)) };
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
		await w.u53(anchor.absolute);
		await w.u53(0);
	} else if (anchor?.relative !== undefined) {
		await w.u53(2);
		await w.u53(anchor.relative);
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
		await w.u53(span.endGroup !== undefined ? span.endGroup + 1 : 0);
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
		await w.u53(startGroup);
		await w.u53(startFrame);
	}
	if (endGroup === undefined) {
		await w.u53(0);
	} else {
		await w.u53(1);
		await w.u53(endFrame === undefined ? endGroup + 1 : endGroup);
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
	if (msg.endGroup !== undefined) range.end = { absolute: msg.endGroup + 1 };
	return [range];
}

/** A position as a comparable pair, group then frame. */
type Point = [group: number, frame: number];

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
			const point: Point = [await r.u53(), await r.u53()];
			return { point, anchor: { absolute: end && point[1] > 0 ? point[0] + 1 : point[0] } };
		}
		case 2:
			return { anchor: { relative: await r.u53() } };
		case 3:
			return { anchor: { time: await r.u53() } };
		default:
			throw new ProtocolViolation(`unknown range anchor: ${kind}`);
	}
}

/**
 * The inverse of {@link encodeSpan}. A lite-07 list decodes as its ranges and as the one span
 * covering every range in it, which the subscriber narrows again.
 */
async function decodeSpan(r: Reader, version: Version): Promise<Span & { ranges?: WireRange[] }> {
	if (!hasRanges(version)) {
		const startGroup = await decodeStartGroup(r, version);
		const endGroup = (await r.u53()) || undefined;
		const end = endGroup !== undefined ? endGroup - 1 : undefined;
		const frames = await decodeFrameBounds(r, version, startGroup, end);
		return { startGroup: canonicalStartGroup(version, startGroup, frames.startFrame), endGroup: end, ...frames };
	}

	const count = await r.u53();
	if (count === 0) throw new ProtocolViolation("a subscription asks for no range");
	let start: Point | undefined;
	let end: Point | undefined;
	let floor = true;
	let live = false;
	const ranges: WireRange[] = [];
	for (let i = 0; i < count; i++) {
		const from = await decodeAnchor(r, false);
		const to = await decodeAnchor(r, true);
		const range: WireRange = {};
		if (from.anchor) range.start = from.anchor;
		if (to.anchor) range.end = to.anchor;
		ranges.push(range);
		if (from.point === undefined) floor = false;
		else if (start === undefined || before(from.point, start)) start = from.point;
		if (to.point === undefined) live = true;
		else if (end === undefined || before(end, to.point)) end = to.point;
	}
	if (!floor) start = undefined;
	if (live) end = undefined;
	// An exclusive end at the very first position names nothing.
	if (end !== undefined && end[0] === 0 && end[1] === 0) throw new ProtocolViolation(EMPTY_RANGE);

	const startFrame = start?.[1] ?? 0;
	return {
		ranges,
		startGroup: canonicalStartGroup(version, start?.[0], startFrame),
		startFrame,
		endGroup: end === undefined ? undefined : end[1] === 0 ? end[0] - 1 : end[0],
		endFrame: end === undefined || end[1] === 0 ? undefined : end[1] - 1,
	};
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
export function inclusiveGroupEnd(exclusive?: number): number | undefined {
	if (exclusive === undefined) return undefined;
	if (exclusive === 0) throw new Error(EMPTY_RANGE);
	return exclusive - 1;
}

export class SubscribeUpdate {
	/** A join requesting this group or later, judged against the local refresh ratio. */
	wantedGroup?: number;
	priority: number;
	/** See {@link Subscribe.order}. */
	order: Order;
	/** Subscriber max delay in milliseconds; zero skips once a newer group is available. */
	maxDelay: number;
	startGroup?: number;
	endGroup?: number;
	/** See {@link Subscribe.startFrame}. */
	startFrame: number;
	/** See {@link Subscribe.endFrame}. */
	endFrame?: number;
	/** See {@link Subscribe.ranges}. */
	ranges?: WireRange[];

	constructor(props: {
		priority: number;
		wantedGroup?: number;
		order?: Order;
		maxDelay?: number;
		startGroup?: number;
		endGroup?: number;
		startFrame?: number;
		endFrame?: number;
		ranges?: WireRange[];
	}) {
		if (props.wantedGroup !== undefined && (!Number.isSafeInteger(props.wantedGroup) || props.wantedGroup < 0))
			throw new RangeError("wanted group must be a safe non-negative integer");
		this.wantedGroup = props.wantedGroup;
		this.priority = props.priority;
		this.order = props.order ?? "descending";
		this.maxDelay = props.maxDelay ?? 0;
		this.startGroup = props.startGroup;
		this.endGroup = props.endGroup;
		this.startFrame = props.startFrame ?? 0;
		this.endFrame = props.endFrame;
		this.ranges = props.ranges;
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
				await encodeSpan(w, version, this, this.ranges);
				if (hasRefresh(version))
					await w.u62(this.wantedGroup === undefined ? 0n : BigInt(this.wantedGroup) + 1n);
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
				const span = await decodeSpan(r, version);
				const wanted = hasRefresh(version) ? await r.u62() : 0n;
				if (wanted > BigInt(Number.MAX_SAFE_INTEGER) + 1n)
					throw new RangeError("wanted group exceeds safe integer");
				return new SubscribeUpdate({
					wantedGroup: wanted === 0n ? undefined : Number(wanted - 1n),
					priority,
					order,
					maxDelay,
					...span,
				});
			}
		}
	}

	async encode(w: Writer, version: Version): Promise<void> {
		if (this.wantedGroup !== undefined && (!Number.isSafeInteger(this.wantedGroup) || this.wantedGroup < 0))
			throw new RangeError("wanted group must be a safe non-negative integer");
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
	epoch?: Epoch.Valid;
	track: string;
	priority: number;
	/**
	 * Which pending group to send first. Lite-07+; earlier wires decode as descending and
	 * cannot ask for anything else.
	 */
	order: Order;
	/** Subscriber max delay in milliseconds; zero skips once a newer group is available. */
	maxDelay: number;

	startGroup?: number;
	endGroup?: number;

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
		epoch?: Epoch.Valid;
		track: string;
		priority: number;
		order?: Order;
		maxDelay?: number;
		startGroup?: number;
		endGroup?: number;
		startFrame?: number;
		endFrame?: number;
		ranges?: WireRange[];
	}) {
		this.id = props.id;
		this.broadcast = props.broadcast;
		this.epoch = props.epoch;
		this.track = props.track;
		this.priority = props.priority;
		this.order = props.order ?? "descending";
		this.maxDelay = props.maxDelay ?? 0;
		this.startGroup = props.startGroup;
		this.endGroup = props.endGroup;
		this.startFrame = props.startFrame ?? 0;
		this.endFrame = props.endFrame;
		this.ranges = props.ranges;
	}

	async #encode(w: Writer, version: Version) {
		await w.u62(this.id);
		await w.string(Path.encode(this.broadcast));
		await encodeEpoch(w, version, this.epoch);
		await w.string(this.track);
		await w.u8(toWire(this.priority));

		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				break;
			default:
				await encodeOrder(w, version, this.order);
				await w.u53(this.maxDelay);
				await encodeSpan(w, version, this, this.ranges);
				break;
		}
	}

	static async #decode(r: Reader, version: Version): Promise<Subscribe> {
		const id = await r.u62();
		const broadcast = Path.decode(await r.string());
		const epoch = await decodeEpoch(r, version);
		const track = await r.string();
		const priority = fromWire(await r.u8());

		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				return new Subscribe({ id, broadcast, track, priority });
			default: {
				const order = await decodeOrder(r, version);
				const maxDelay = await r.u53();
				const span = await decodeSpan(r, version);
				return new Subscribe({
					id,
					broadcast,
					epoch,
					track,
					priority,
					order,
					maxDelay,
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
 * {@link SubscribeEnd}; the immutable codec/timescale/cache moved to TRACK_INFO.
 */
export class SubscribeOk {
	priority: number;
	/** Accepted subscriber max delay in milliseconds. */
	maxDelay: number;
	startGroup?: number;
	endGroup?: number;

	constructor({
		priority = 0,
		maxDelay = 0,
		startGroup = undefined,
		endGroup = undefined,
	}: {
		priority?: number;
		maxDelay?: number;
		startGroup?: number;
		endGroup?: number;
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
				await w.u53(this.startGroup !== undefined ? this.startGroup + 1 : 0);
				await w.u53(this.endGroup !== undefined ? this.endGroup + 1 : 0);
				break;
		}
	}

	static async #decode(version: Version, r: Reader): Promise<SubscribeOk> {
		let priority: number | undefined;
		let maxDelay: number | undefined;
		let startGroup: number | undefined;
		let endGroup: number | undefined;

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
				startGroup = await r.u53();
				endGroup = await r.u53();
				break;
		}

		return new SubscribeOk({
			priority,
			maxDelay,
			startGroup: startGroup !== undefined && startGroup > 0 ? startGroup - 1 : undefined,
			endGroup: endGroup !== undefined && endGroup > 0 ? endGroup - 1 : undefined,
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
 * Resolves the absolute start group of a Draft-05+ subscription. The first message
 * the publisher sends, once the start group is known. A value greater than the
 * requested start implicitly drops the leading range.
 *
 * There is no start frame: a partial group is only served to a subscriber that asked
 * for one, so delivery begins either at the requested `startFrame` (when this is the
 * requested group) or at frame 0 (when the publisher resolved to a later one).
 */
export class SubscribeStart {
	group: number;

	/**
	 * The publisher's largest (group, frame) when it answered, or `undefined` for a track
	 * with nothing yet. Draft-07+ only; not on the wire before, where it decodes as `undefined`.
	 */
	largest?: Position;

	constructor(group: number, largest?: Position) {
		this.group = group;
		this.largest = largest;
	}

	async encode(w: Writer, version: Version): Promise<void> {
		return Message.encode(w, async (w) => {
			await w.u53(this.group);
			if (!hasLargest(version)) return;
			// Group + 1, so 0 is a track with nothing yet; the frame follows only otherwise.
			if (this.largest === undefined) {
				await w.u53(0);
			} else {
				await w.u53(this.largest.group + 1);
				await w.u53(this.largest.frame);
			}
		});
	}

	static async decode(r: Reader, version: Version): Promise<SubscribeStart> {
		return Message.decode(r, async (r) => {
			const group = await r.u53();
			if (!hasLargest(version)) return new SubscribeStart(group);
			const largest = await r.u53();
			if (largest === 0) return new SubscribeStart(group);
			return new SubscribeStart(group, { group: largest - 1, frame: await r.u53() });
		});
	}
}

/**
 * Signals that no group at or after `group` (exclusive upper bound) will be produced
 * on a Draft-05+ subscription. `0` means the track ended before producing any groups.
 */
export class SubscribeEnd {
	/** The exclusive final group sequence: the first sequence that will never be produced. */
	group: number;

	constructor(group: number) {
		this.group = group;
	}

	async encode(w: Writer, _version: Version): Promise<void> {
		return Message.encode(w, async (w) => {
			await w.u53(this.group);
		});
	}

	static async decode(r: Reader, _version: Version): Promise<SubscribeEnd> {
		return Message.decode(r, async (r) => new SubscribeEnd(await r.u53()));
	}
}

/// Indicates that one or more groups have been dropped.
///
/// Draft-03+; publishers send it on Draft-05 and later.
export class SubscribeDrop {
	start: number;
	end: number;
	error: number;

	constructor(props: { start: number; end: number; error: number }) {
		this.start = props.start;
		this.end = props.end;
		this.error = props.error;
	}

	/** Name `[start, end]` given up with `err`, as `version` carries it. */
	static given(start: number, end: number, err: unknown, version: Version): SubscribeDrop {
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
		if (this.start - this.end > 1) throw new RangeError("invalid dropped range");
		await w.u53(this.start);
		await w.u53(this.end);
		await w.u53(this.error);
	}

	static async #decode(r: Reader): Promise<SubscribeDrop> {
		const drop = new SubscribeDrop({ start: await r.u53(), end: await r.u53(), error: await r.u53() });
		if (drop.start - drop.end > 1) throw new ProtocolViolation("invalid dropped range");
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
 * - Draft-05+: `0x0` SUBSCRIBE_START, `0x1` SUBSCRIBE_END, `0x2` SUBSCRIBE_DROP
 *   (SUBSCRIBE_OK was removed; acceptance is implicit).
 */
export type SubscribeResponse =
	| { ok: SubscribeOk }
	| { start: SubscribeStart }
	| { end: SubscribeEnd }
	| { drop: SubscribeDrop };

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
				throw new Error("SUBSCRIBE_START/END not supported for this version");
			}
			break;
		default:
			// Draft-05+: SUBSCRIBE_OK is gone; START/END/DROP carry the resolved range.
			if ("start" in resp) {
				await w.u53(0x0);
				await resp.start.encode(w, version);
			} else if ("end" in resp) {
				await w.u53(0x1);
				await resp.end.encode(w, version);
			} else if ("drop" in resp) {
				await w.u53(0x2);
				await resp.drop.encode(w);
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
					return { end: await SubscribeEnd.decode(r, version) };
				case 0x2:
					return { drop: await SubscribeDrop.decode(r) };
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

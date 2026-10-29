import type * as Epoch from "../epoch.ts";
import { fromTransport, ProtocolViolation, StreamCode, StreamError, toStreamCode } from "../error.ts";
import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import type { Order, Position } from "../track.ts";
import { encodeLeadingOnes } from "../varint.ts";
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
 * Encode the span after the max delay. Before lite-07 that is `Group Start`, `Group End`,
 * and the frame bounds; lite-07 carries a list of ranges, and this span is its only one.
 */
async function encodeSpan(w: Writer, version: Version, span: Span) {
	if (!hasRanges(version)) {
		await encodeStartGroup(w, version, span.startGroup);
		await w.u53(span.endGroup !== undefined ? span.endGroup + 1 : 0);
		await encodeFrameBounds(w, version, span);
		return;
	}

	await encodeRanges(w, [spanRange(span)]);
}

/**
 * A lite-07 range with each end resolved to a position, or `undefined` for none. A relative
 * or time end decodes as `undefined` too: this publisher resolves neither, so it serves from
 * the oldest group its budget allows, or through the live edge.
 */
export interface Range {
	from?: Point;
	to?: Point;
}

/** The one range a span names, with its exclusive end. */
function spanRange({ startGroup, startFrame, endGroup, endFrame }: Span): Range {
	if ((startFrame !== 0 && startGroup === undefined) || (endFrame !== undefined && endGroup === undefined)) {
		throw new Error("frame bound without a group bound");
	}
	return {
		from: startGroup === undefined ? undefined : [startGroup, startFrame],
		to: endGroup === undefined ? undefined : endFrame === undefined ? [endGroup + 1, 0] : [endGroup, endFrame + 1],
	};
}

function sameRange(a: Range, b: Range): boolean {
	const same = (x?: Point, y?: Point) => x?.[0] === y?.[0] && x?.[1] === y?.[1];
	return same(a.from, b.from) && same(a.to, b.to);
}

/** Write a lite-07 `Range Count` and its ranges. */
async function encodeRanges(w: Writer, ranges: Range[]) {
	await w.u53(ranges.length);
	for (const { from, to } of ranges) {
		// Each end is a type, then its fields: 0 is none (the oldest group as a start, the live
		// edge as an end) and 1 an absolute position, which an end stops just before.
		for (const end of [from, to]) {
			if (end === undefined) {
				await w.u53(0);
			} else {
				await w.u53(1);
				await w.u53(end[0]);
				await w.u53(end[1]);
			}
		}
	}
}

/** A decoded range and the bytes it took on the wire. */
interface Sized {
	range: Range;
	size: number;
}

/** Read a lite-07 `Range Count` and its ranges. */
async function decodeRanges(r: Reader): Promise<Sized[]> {
	const count = await r.u53();
	const ranges: Sized[] = [];
	for (let i = 0; i < count; i++) {
		const from = await decodeAnchor(r);
		const to = await decodeAnchor(r);
		ranges.push({ range: { from: from.point, to: to.point }, size: from.size + to.size });
	}
	return ranges;
}

/** The span covering every range, as a pre-07 publisher serves it. */
function cover(version: Version, ranges: Iterable<Range>): Span {
	let start: Point | undefined;
	let end: Point | undefined;
	let floor = true;
	let live = false;
	for (const { from, to } of ranges) {
		if (from === undefined) floor = false;
		else if (start === undefined || before(from, start)) start = from;
		if (to === undefined) live = true;
		else if (end === undefined || before(end, to)) end = to;
	}
	if (!floor) start = undefined;
	if (live) end = undefined;
	// An exclusive end at the very first position names nothing.
	if (end !== undefined && end[0] === 0 && end[1] === 0) throw new ProtocolViolation(EMPTY_RANGE);

	const startFrame = start?.[1] ?? 0;
	return {
		startGroup: canonicalStartGroup(version, start?.[0], startFrame),
		startFrame,
		endGroup: end === undefined ? undefined : end[1] === 0 ? end[0] - 1 : end[0],
		endFrame: end === undefined || end[1] === 0 ? undefined : end[1] - 1,
	};
}

/** The most bytes a subscription's live ranges may take: what one SUBSCRIBE can carry. */
const MAX_LIVE_RANGE_BYTES = 65_535;

/**
 * A subscription's live lite-07 ranges by Range ID, which both ends of a Subscribe Stream
 * keep: the subscriber to write each update as an edit, the publisher to resolve it.
 *
 * The SUBSCRIBE's ranges take Range IDs from 0 in order, and each range an update adds
 * takes the next.
 */
export class LiveRanges {
	#next = 0;
	#live = new Map<number, Sized>();
	#size = 0;

	constructor(ranges: Sized[]) {
		this.#add(ranges);
	}

	/** The live ranges of a subscription that sent `span`. */
	static sent(span: Span): LiveRanges {
		return new LiveRanges([{ range: spanRange(span), size: 0 }]);
	}

	#add(ranges: Sized[]) {
		for (const sized of ranges) {
			this.#live.set(this.#next++, sized);
			this.#size += sized.size;
		}
	}

	/** Resolve a received edit into the span covering the live ranges. */
	apply(version: Version, remove: number[], add: Sized[]): Span {
		for (const id of remove) {
			const sized = this.#live.get(id);
			// An id never assigned, or already retired (including earlier in this list).
			if (!sized) throw new ProtocolViolation(`unknown range id: ${id}`);
			this.#live.delete(id);
			this.#size -= sized.size;
		}
		this.#add(add);
		if (this.#live.size === 0) throw new ProtocolViolation("a subscription asks for no range");
		if (this.#size > MAX_LIVE_RANGE_BYTES) throw new ProtocolViolation("live ranges exceed one SUBSCRIBE");
		return cover(
			version,
			[...this.#live.values()].map(({ range }) => range),
		);
	}

	/** The edit that turns the live ranges into `span`'s alone, which becomes the live one. */
	edit(span: Span): { remove: number[]; add: Range[] } {
		const range = spanRange(span);
		const remove: number[] = [];
		let kept = false;
		for (const [id, { range: live }] of this.#live) {
			if (!kept && sameRange(live, range)) kept = true;
			else remove.push(id);
		}
		for (const id of remove) this.#live.delete(id);
		if (kept) return { remove, add: [] };
		this.#add([{ range, size: 0 }]);
		return { remove, add: [range] };
	}
}

/** A position as a comparable pair, group then frame. */
type Point = [group: number, frame: number];

function before(a: Point, b: Point): boolean {
	return a[0] < b[0] || (a[0] === b[0] && a[1] < b[1]);
}

/**
 * Decode one lite-07 range end, `undefined` for none, and the bytes it took. A relative or
 * time anchor is also `undefined`; see {@link Range}.
 */
async function decodeAnchor(r: Reader): Promise<{ point?: Point; size: number }> {
	const read = async () => {
		const value = await r.u53();
		size += encodeLeadingOnes(value).length;
		return value;
	};
	let size = 0;
	const kind = await read();
	switch (kind) {
		case 0:
			return { size };
		case 1:
			return { point: [await read(), await read()], size };
		case 2:
		case 3:
			await read();
			return { size };
		default:
			throw new ProtocolViolation(`unknown range anchor: ${kind}`);
	}
}

/**
 * The inverse of {@link encodeSpan}. A lite-07 list decodes as the one span covering every
 * range in it, which the subscriber narrows again, and the live ranges its updates edit.
 */
async function decodeSpan(r: Reader, version: Version): Promise<Span & { live?: LiveRanges }> {
	if (!hasRanges(version)) {
		const startGroup = await decodeStartGroup(r, version);
		const endGroup = (await r.u53()) || undefined;
		const end = endGroup !== undefined ? endGroup - 1 : undefined;
		const frames = await decodeFrameBounds(r, version, startGroup, end);
		return { startGroup: canonicalStartGroup(version, startGroup, frames.startFrame), endGroup: end, ...frames };
	}

	const live = new LiveRanges(await decodeRanges(r));
	return { ...live.apply(version, [], []), live };
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

/**
 * Exclusive model/local cap from a decoded inclusive last group.
 *
 * The wire's `Group End` is inclusive once decoded; the track model and `withRange` are
 * exclusive. `undefined` stays unbounded.
 */
export function exclusiveGroupEnd(inclusive?: number): number | undefined {
	return inclusive === undefined ? undefined : inclusive + 1;
}

/** The error for a requested range the wire cannot carry; see {@link emptyRange}. */
export const EMPTY_RANGE = "empty subscription range cannot be encoded";

/**
 * Inclusive last group a Subscribe message carries, from an exclusive model end.
 *
 * Empty (`0`) cannot be encoded: the wire's 0 means unbounded. Callers refuse an empty
 * requested range with {@link emptyRange} before reaching here.
 */
export function inclusiveGroupEnd(exclusive?: number): number | undefined {
	if (exclusive === undefined) return undefined;
	if (exclusive === 0) throw new Error(EMPTY_RANGE);
	return exclusive - 1;
}

/**
 * Whether a requested range asks for nothing.
 *
 * The wire has no encoding for one: its bounds are inclusive, so flooring the end would
 * either hand back the group the caller excluded (0 means unbounded) or invert the
 * range once the two bounds meet. An absent start is the live edge, so it only empties
 * the range when the end is 0.
 */
export function emptyRange({ startGroup, endGroup }: { startGroup?: number; endGroup?: number }): boolean {
	return endGroup !== undefined && (startGroup ?? 0) >= endGroup;
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

	constructor(props: {
		priority: number;
		wantedGroup?: number;
		order?: Order;
		maxDelay?: number;
		startGroup?: number;
		endGroup?: number;
		startFrame?: number;
		endFrame?: number;
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
	}

	async #encode(w: Writer, version: Version, live?: LiveRanges) {
		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				await w.u8(toWire(this.priority));
				break;
			default:
				await w.u8(toWire(this.priority));
				await encodeOrder(w, version, this.order);
				await w.u53(this.maxDelay);
				if (hasRanges(version)) {
					if (!live) throw new Error("a lite-07 update edits the live ranges");
					const { remove, add } = live.edit(this);
					await w.u53(remove.length);
					for (const id of remove) await w.u53(id);
					await encodeRanges(w, add);
				} else {
					await encodeSpan(w, version, this);
				}
				if (hasRefresh(version))
					await w.u62(this.wantedGroup === undefined ? 0n : BigInt(this.wantedGroup) + 1n);
				break;
		}
	}

	static async #decode(r: Reader, version: Version, live?: LiveRanges): Promise<SubscribeUpdate> {
		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				return new SubscribeUpdate({ priority: fromWire(await r.u8()) });
			default: {
				const priority = fromWire(await r.u8());
				const order = await decodeOrder(r, version);
				const maxDelay = await r.u53();
				let span: Span;
				if (hasRanges(version)) {
					if (!live) throw new Error("a lite-07 update edits the live ranges");
					const remove: number[] = [];
					for (let i = await r.u53(); i > 0; i--) remove.push(await r.u53());
					span = live.apply(version, remove, await decodeRanges(r));
				} else {
					span = await decodeSpan(r, version);
				}
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

	/** Lite-07 writes the ranges as an edit of `live`, the ranges the publisher holds. */
	async encode(w: Writer, version: Version, live?: LiveRanges): Promise<void> {
		if (this.wantedGroup !== undefined && (!Number.isSafeInteger(this.wantedGroup) || this.wantedGroup < 0))
			throw new RangeError("wanted group must be a safe non-negative integer");
		return Message.encode(w, (w) => this.#encode(w, version, live));
	}

	/** Lite-07 resolves the ranges as an edit of `live`, the ranges the SUBSCRIBE carried. */
	static async decode(r: Reader, version: Version, live?: LiveRanges): Promise<SubscribeUpdate> {
		return Message.decode(r, (r) => SubscribeUpdate.#decode(r, version, live));
	}

	static async decodeMaybe(r: Reader, version: Version, live?: LiveRanges): Promise<SubscribeUpdate | undefined> {
		return Message.decodeMaybe(r, (r) => SubscribeUpdate.#decode(r, version, live));
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

	/** The ranges a decoded lite-07 SUBSCRIBE carried, which its updates edit. */
	live?: LiveRanges;

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
		live?: LiveRanges;
	}) {
		this.id = props.id;
		this.live = props.live;
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
				await encodeSpan(w, version, this);
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

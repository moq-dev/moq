import { Duration as ClockDuration } from "@moq/clock";
import { fromTransport, ProtocolViolation, StreamCode, StreamError, toStreamCode } from "../error.ts";
import type * as Identity from "../identity.ts";
import { NO_MAX_DELAY } from "../internal.ts";
import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import type { Order, Position, Range, Start } from "../track.ts";
import { UNBOUNDED } from "../util/key.ts";
import { lengthLeadingOnes, parts, split } from "../util/varint.ts";
import { EMPTY, liveMillis, rangeBudget, rangeTime } from "../window.ts";
import * as Duration from "./duration.ts";
import { decodeNode, decodePin, encodeNode, encodePin } from "./identity.ts";
import * as Message from "./message.ts";
import { fromWire, toWire } from "./priority.ts";
import {
	hasDropCode,
	hasEndError,
	hasFrameBounds,
	hasGroupOrder,
	hasGroupSpan,
	hasMeta,
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
 * One end of a lite-07 range: a model time (`time`), a group by its start (`absolute`), or a live
 * join (`live`), as the Rust `lite::Anchor` holds. A group's start is its time, so `absolute` goes
 * out as a time, an end (exclusive) as the time just before it, and decodes as one. One partway
 * through the group, at a nonzero `frame`, goes out as a Position: a start from that frame, an end
 * before it. A live join is a start only, and so is one already placed (`joined`).
 */
export type WireAnchor =
	| { absolute: bigint; frame?: number; time?: never; live?: never; joined?: never }
	| { time: bigint; absolute?: never; live?: never; joined?: never }
	| { live: WireLive; absolute?: never; time?: never; joined?: never }
	| { joined: WireJoined; absolute?: never; time?: never; live?: never };

/**
 * A live join already placed, on the wire: the group it starts at by its start, the frame there,
 * 0 for the whole group, and the `max` its groups expire past, `null` for no limit beyond the
 * publisher's, in whole milliseconds. A route switch asks a new upstream for a join an earlier one
 * placed so. Lite-07 only. Mirrors the Rust `lite::Anchor::Joined`.
 */
export interface WireJoined {
	absolute: bigint;
	frame: number;
	max: number | null;
}

/**
 * A live join on the wire: its lag `min` behind the live edge, `null` for no lag limit, and the
 * `max` its groups expire past, `null` for no limit beyond the publisher's, both in whole
 * milliseconds.
 */
export interface WireLive {
	min: number | null;
	max: number | null;
}

/** A lite-07 range as the wire carries it; see {@link WireAnchor}. */
export interface WireRange {
	start?: WireAnchor;
	end?: WireAnchor;
}

/**
 * The wire form of model `ranges`: the same times, a time being a group start, and live joins by lag
 * and max, or, `placed` already at a group by index, as `joined` there (see {@link WireJoined}).
 */
export function toWireRanges(ranges: readonly Range[], placed?: ReadonlyMap<number, bigint>): WireRange[] {
	return ranges.map((range, index) => {
		const out: WireRange = {};
		const time = rangeTime(range);
		const min = liveMillis(range);
		const group = placed?.get(index);
		if (time !== undefined) out.start = group === undefined ? { time } : { absolute: group, frame: 0 };
		else if (min !== undefined) {
			const budget = rangeBudget(range);
			const max = budget < NO_MAX_DELAY ? budget : null;
			out.start = group === undefined ? { live: { min, max } } : { joined: { absolute: group, frame: 0, max } };
		}
		if (range.end !== undefined) out.end = { time: range.end };
		return out;
	});
}

/**
 * The lite-07 Anchor Type and values of a range's start or `end`: None (0), Time (1), Position (2),
 * Live (3), or Joined (4). Both time ends are inclusive, so an absolute end, exclusive, goes out as
 * the time just before it; one at group 0 holds nothing a time can say. An end or start partway
 * through a group is a Position: its start, then the frame. A live join goes out as its lag then
 * its max in milliseconds plus one, 0 for no limit, and only as a start; one already placed as its
 * group's start, the frame, which may be 0, then its max as a live join carries it.
 */
function wireAnchor(anchor: WireAnchor | undefined, end: boolean): [number, ...bigint[]] {
	if (anchor?.joined !== undefined) {
		if (end) throw new Error("a range ends only at a group");
		const { absolute, frame, max } = anchor.joined;
		if (max !== null && (!Number.isSafeInteger(max) || max < 0))
			throw new RangeError("duration exceeds safe milliseconds");
		return [4, absolute, BigInt(frame), max === null ? 0n : BigInt(max) + 1n];
	}
	if (anchor?.live !== undefined) {
		if (end) throw new Error("a range ends only at a group");
		const { min, max } = anchor.live;
		for (const limit of [min, max]) {
			if (limit !== null && (!Number.isSafeInteger(limit) || limit < 0))
				throw new RangeError("duration exceeds safe milliseconds");
		}
		// Each plus one, so zero says no limit.
		return [3, min === null ? 0n : BigInt(min) + 1n, max === null ? 0n : BigInt(max) + 1n];
	}
	if (anchor?.absolute !== undefined) {
		return [2, anchor.absolute, BigInt(anchor.frame ?? 0)];
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
 * starting at or after it, which the publisher's floor keeps (see `cursorFloor`), and its span with
 * no start takes `legacy` (see {@link legacyStart}).
 */
export function requestedRanges(msg: Span & { ranges?: WireRange[] }, legacy?: Start): Range[] {
	if (msg.ranges) return msg.ranges.map(requestedRange);
	const range: Range = {};
	const max = legacyMax(legacy);
	// One max age bounds a named start too: a live join placed there (see {@link requestedPlaced}).
	if (msg.startGroup !== undefined && max !== undefined) range.start = { live: { min: ClockDuration.ZERO, max } };
	else if (msg.startGroup !== undefined) range.start = msg.startGroup;
	else if (legacy !== undefined) range.start = legacy;
	if (msg.endGroup !== undefined) range.end = msg.endGroup;
	return [range];
}

/**
 * The start a span with none takes on a `version` before lite-07, where one `Subscriber Max Age`
 * (`maxDelay` milliseconds, `null` for none) bounds the whole subscription; undefined on lite-07,
 * which carries one on each live start instead, and for the oldest group.
 *
 * An absent start takes every group still within that max age, or every group with none, as
 * lite-01/02 never carry one. Lite-06 resolves it there. Before lite-06 an absent start is the
 * latest group, which the publisher places as the floor once the source's feed says where it
 * starts (see `cursorFloor`). A named start with a max age is a live join placed there; without
 * one, a history floor bounded by the publisher's max age. Mirrors the Rust `Bounds::legacy`.
 */
export function legacyStart(version: Version, maxDelay: number | null): Start | undefined {
	if (hasRanges(version) || maxDelay === null) return undefined;
	// A join with no lag limit takes every group still within its max. Mirrors the Rust `Start::within`.
	return { live: { min: null, max: ClockDuration.millis(maxDelay) } };
}

/** The max age a pre-07 `legacy` start carries, undefined when it carries none. */
function legacyMax(legacy: Start | undefined): ClockDuration | undefined {
	if (legacy === undefined || typeof legacy === "bigint" || legacy.live.max === null) return undefined;
	return legacy.live.max ?? ClockDuration.ZERO;
}

/**
 * The group each live join already placed among the ranges `msg` asks for starts at, by its index:
 * a lite-07 {@link WireJoined}, or an older wire's named start under a max age (see
 * {@link requestedRanges}). See `Subscription.placed`.
 */
export function requestedPlaced(msg: Span & { ranges?: WireRange[] }, legacy?: Start): Map<number, bigint> {
	const placed = new Map<number, bigint>();
	if (!msg.ranges) {
		if (msg.startGroup !== undefined && legacyMax(legacy) !== undefined) placed.set(0, msg.startGroup);
		return placed;
	}
	for (const [index, range] of msg.ranges.entries()) {
		const at = range.start?.joined?.absolute ?? range.start?.absolute;
		if (at !== undefined) placed.set(index, at);
	}
	return placed;
}

/** The model range one lite-07 range asks for; see {@link requestedRanges}. */
export function requestedRange(range: WireRange): Range {
	// Empty, as group bounds say it: keep its place, covering no group.
	const start = range.start?.absolute;
	const end = range.end && endGroup(range.end);
	if (frameEmpty(range)) return { ...EMPTY };
	if (end === 0n) return { ...EMPTY };
	const out: Range = {};
	const live = range.start?.live;
	const joined = range.start?.joined;
	const maxOf = (max: number | null) => (max === null || max >= NO_MAX_DELAY ? null : ClockDuration.millis(max));
	if (live !== undefined)
		out.start = { live: { min: live.min === null ? null : ClockDuration.millis(live.min), max: maxOf(live.max) } };
	// Placed already, so its lag places nothing: it starts where it was placed (see
	// {@link requestedPlaced}) and expires as a live join under its max.
	else if (joined !== undefined) out.start = { live: { min: ClockDuration.ZERO, max: maxOf(joined.max) } };
	else if (range.start) out.start = start ?? range.start.time;
	// An absolute end is exclusive; every model end includes its group.
	if (range.end) out.end = end === undefined ? range.end.time : end - 1n;
	return out;
}

/**
 * Decode one lite-07 range end, `undefined` for none: a time, inclusive as an end, a position
 * partway through a group, never at frame 0, which a time says, a live join's lag then max in
 * milliseconds, a start only, or one already placed, its group's start, a frame that may be 0, then
 * its max, a start only too. Any other Anchor Type is a protocol violation.
 */
async function decodeAnchor(r: Reader, end: boolean): Promise<WireAnchor | undefined> {
	const kind = await r.u53();
	switch (kind) {
		case 0:
			return undefined;
		case 1:
			return { time: await r.u62() };
		case 2: {
			const absolute = await r.u62();
			const frame = await r.u53();
			return { absolute, frame };
		}
		case 3:
			if (end) throw new ProtocolViolation("a range ends at a live join");
			return {
				live: {
					min: (await Duration.decodeOptional(r)) ?? null,
					max: (await Duration.decodeOptional(r)) ?? null,
				},
			};
		case 4: {
			if (end) throw new ProtocolViolation("a range ends at a live join");
			const absolute = await r.u62();
			const frame = await r.u53();
			return { joined: { absolute, frame, max: (await Duration.decodeOptional(r)) ?? null } };
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
	const from = range.start?.absolute ?? range.start?.time ?? range.start?.joined?.absolute;
	const end = range.end;
	const to = end && (endGroup(end) ?? (end.time === undefined ? undefined : end.time + 1n));
	return [from, to];
}

/**
 * The exclusive group an absolute end stops before, `undefined` for a time: past its group when
 * it stops partway through it.
 */
function endGroup(end: WireAnchor): bigint | undefined {
	if (end.absolute === undefined) return undefined;
	return end.frame ? end.absolute + 1n : end.absolute;
}

/** Whether a range ends at or before its start, frames included: empty, covering no group. */
function frameEmpty(range: WireRange): boolean {
	return range.end !== undefined && endsBefore(range.end, range.start);
}

/** Whether absolute `end`, exclusive, stops at or before `start`, frames included. */
function endsBefore(end: WireAnchor, start: WireAnchor | undefined): boolean {
	if (end.absolute === undefined) return false;
	const from = start?.absolute !== undefined ? start : start?.joined;
	if (from === undefined) return false;
	if (end.absolute !== from.absolute) return end.absolute < from.absolute;
	return (end.frame ?? 0) <= (from.frame ?? 0);
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
	const positions: [bigint, number][] = [];
	for (const range of ranges) {
		if (frameEmpty(range)) continue;
		// A Position start, or a placed join's, partway through its group.
		const start = range.start?.joined ?? range.start;
		// One ending at or before the group it starts partway through covers nothing.
		const [, to] = rangeBounds(range);
		if (start?.absolute !== undefined && start.frame && (to === undefined || to > start.absolute))
			positions.push([start.absolute, start.frame]);
	}
	return onlyCovered(positions, ranges);
}

/**
 * The frame each lite-07 Position end stops its group before, by group, for each group no other
 * range may cover, as {@link positionFrames} keeps starts. Mirrors the Rust `stops`.
 */
export function positionStops(ranges: readonly WireRange[]): Map<bigint, number> {
	const positions: [bigint, number][] = [];
	for (const range of ranges) {
		if (frameEmpty(range)) continue;
		const end = range.end;
		// One starting past the group it stops partway through covers nothing.
		const [from, to] = rangeBounds(range);
		if (end?.absolute !== undefined && end.frame && (from === undefined || to === undefined || from < to))
			positions.push([end.absolute, end.frame]);
	}
	return onlyCovered(positions, ranges);
}

/** The `positions` whose group no range in `ranges` but their own may cover, by group. */
function onlyCovered(positions: readonly [bigint, number][], ranges: readonly WireRange[]): Map<bigint, number> {
	const frames = new Map<bigint, number>();
	if (positions.length === 0) return frames;
	// A range may cover a group unless it starts past it or ends at or before it. One that
	// starts at or past its end covers none, so with it left out, no range does both and the
	// ranges covering a group are the rest less those two counts.
	const starts: bigint[] = [];
	const ends: bigint[] = [];
	let count = 0;
	for (const range of ranges) {
		if (frameEmpty(range)) continue;
		// No start, or no end, may cover it.
		const [from, to] = rangeBounds(range);
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
		if (anchor?.live !== undefined) return `l${anchor.live.min}/${anchor.live.max ?? "-"}`;
		if (anchor?.joined !== undefined)
			return `j${anchor.joined.absolute}.${anchor.joined.frame}/${anchor.joined.max ?? "-"}`;
		return "-";
	};
	return `${anchor(range.start)}:${anchor(range.end)}`;
}

/**
 * A received update's ranges resolved by Range ID: the ones retired, then each one added, then
 * each live join retuned as it now reads. Mirrors the Rust `Applied`.
 */
export interface Applied {
	retired: bigint[];
	added: [bigint, WireRange][];
	retuned: [bigint, WireRange][];
}

/**
 * What a live lite-07 range asks for: the demand's groups, which set where the live feed is, where
 * the publisher starts a time start a copy's readers wait on (a probe), or a fill's groups.
 */
export type RangeKind = "demand" | "probe" | "fill";

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

	/** The ranges a SUBSCRIBE carried. */
	constructor(version: Version, ranges: readonly WireRange[]) {
		this.#version = version;
		this.#add(ranges);
	}

	#add(ranges: readonly WireRange[]) {
		for (const range of ranges) {
			// Only lite-07 numbers ranges on the wire, and only it is held to one SUBSCRIBE.
			if (hasRanges(this.#version)) this.#size += rangeSize(range);
			this.#live.set(this.#next++, range);
		}
	}

	#remove(id: bigint): WireRange | undefined {
		const range = this.#live.get(id);
		if (!range) return undefined;
		if (hasRanges(this.#version)) this.#size -= rangeSize(range);
		this.#live.delete(id);
		return range;
	}

	// Replace the live range with Range ID `id` by `range` in place, keeping Range ID order.
	#replace(id: bigint, range: WireRange) {
		const old = this.#live.get(id);
		if (old && hasRanges(this.#version)) this.#size += rangeSize(range) - rangeSize(old);
		this.#live.set(id, range);
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
	 * each added with the Range ID it took, then each live join retuned to its new max in place,
	 * costing the ranges it names. Throws {@link ProtocolViolation} for an edit it cannot apply.
	 * Before lite-07 an update restates its ranges, so nothing is numbered and the edit is empty.
	 * Mirrors the Rust `LiveRanges::apply`.
	 */
	apply(update: SubscribeUpdate): Applied {
		if (!hasRanges(this.#version)) return { retired: [], added: [], retuned: [] };
		const retired = update.remove ?? [];
		for (const id of retired) {
			// An id never assigned, or already retired (including earlier in this list).
			if (!this.#remove(id)) throw new ProtocolViolation(`unknown range id: ${id}`);
		}
		// Retuned before the adds, so a retune names a range live before this edit.
		const retuned: [bigint, WireRange][] = [];
		for (const [id, max] of update.retune ?? []) {
			// Each live join once: one retuned twice in an edit says nothing a single one doesn't.
			if (retuned.some(([tuned]) => tuned === id)) throw new ProtocolViolation(`range id retuned twice: ${id}`);
			const range = this.#live.get(id);
			const live = range?.start?.live;
			const joined = range?.start?.joined;
			if (range === undefined || (live === undefined && joined === undefined))
				throw new ProtocolViolation(`no live join to retune: ${id}`);
			// One already placed stays where it was placed.
			const tuned: WireRange = {
				...range,
				start: joined ? { joined: { ...joined, max } } : { live: { min: live ? live.min : 0, max } },
			};
			this.#replace(id, tuned);
			retuned.push([id, tuned]);
		}
		const first = this.#next;
		const add = update.ranges ?? [];
		this.#add(add);
		// Asking for nothing is refused like a SUBSCRIBE that does, and a set no SUBSCRIBE
		// could carry like one too large to send.
		if (this.#live.size === 0) throw new ProtocolViolation("a subscription asks for no range");
		if (this.#size > Message.MAX_MESSAGE_SIZE) throw new ProtocolViolation("too many ranges");
		return {
			retired: [...retired],
			added: add.map((range, index) => [first + BigInt(index), range]),
			retuned,
		};
	}

	/**
	 * The edit that turns the live ranges into `ranges`, which become the live ones: the Range IDs
	 * to retire and the ranges to add. A range already live keeps its Range ID, so an update that
	 * changes one range sends only that one, and the publisher keeps where the rest resolved.
	 * Before lite-07 replaced ranges take fresh Range IDs.
	 *
	 * `ranges` are the demand's, then `kinds.probes` probes, then `kinds.fills` asking for fills,
	 * and `kinds.kindOf` names what each live one asks for: a probe or a fill asking for what a
	 * demand range does still takes a range of its own, so each Range ID stays one kind's.
	 * `fillIds` and `probeIds` are the Range IDs the added ones of each took. The live ranges in
	 * `renew` are retired and added again even when wanted, so the publisher resolves them afresh.
	 * A live join wanted again under another max alone, as the same kind, is retuned in place,
	 * keeping its Range ID and so where it was placed. Mirrors the Rust `SubStream::edit`.
	 */
	edit(
		ranges: readonly WireRange[],
		kinds: { probes?: number; fills?: number; kindOf?: (id: bigint) => RangeKind } = {},
		renew: ReadonlySet<bigint> = new Set(),
	): {
		remove: bigint[];
		add: WireRange[];
		retune: [bigint, number | null][];
		fillIds: bigint[];
		probeIds: bigint[];
	} {
		if (ranges.length === 0) throw new Error(EMPTY_RANGE);
		if (!hasRanges(this.#version)) {
			const same =
				ranges.length === this.#live.size &&
				this.ranges().every((range, index) => rangeKey(range) === rangeKey(ranges[index]));
			if (!same) {
				this.#live.clear();
				this.#add(ranges);
			}
			return { remove: [], add: [...ranges], retune: [], fillIds: [], probeIds: [] };
		}
		const probes = kinds.probes ?? 0;
		const demand = ranges.length - (kinds.fills ?? 0) - probes;
		const kindOf = kinds.kindOf ?? (() => "demand");
		const kindAt = (index: number): RangeKind =>
			index < demand ? "demand" : index < demand + probes ? "probe" : "fill";
		const key = (range: WireRange, kind: RangeKind) => `${kind[0]}${rangeKey(range)}`;
		// Each live range keeps at most one wanted copy, so duplicates edit like any other.
		const wanted = new Map<string, number>();
		ranges.forEach((range, index) => {
			const k = key(range, kindAt(index));
			wanted.set(k, (wanted.get(k) ?? 0) + 1);
		});
		const remove: bigint[] = [];
		// The bytes the live ranges take once the edit applies.
		let size = this.#size;
		for (const [id, range] of this.#live) {
			const k = key(range, kindOf(id));
			const count = wanted.get(k) ?? 0;
			if (count > 0 && !renew.has(id)) {
				wanted.set(k, count - 1);
			} else {
				remove.push(id);
				size -= rangeSize(range);
			}
		}
		// In the order asked for, each with its kind, so the publisher numbers them the way this
		// side does.
		const adding: [WireRange, RangeKind][] = [];
		ranges.forEach((range, index) => {
			const kind = kindAt(index);
			const k = key(range, kind);
			const count = wanted.get(k) ?? 0;
			if (count === 0) return;
			wanted.set(k, count - 1);
			adding.push([range, kind]);
		});
		// A live join wanted again under another max alone, its placement kept: one being renewed
		// is placed afresh.
		const retuned: [bigint, WireRange][] = [];
		const kept = remove.filter((id) => {
			const start = this.#live.get(id)?.start;
			const live = start?.live;
			const joined = start?.joined;
			if ((live === undefined && joined === undefined) || renew.has(id)) return true;
			const end = rangeKey({ end: this.#live.get(id)?.end });
			// The same join: one live by its lag, one placed by where it was placed.
			const same = (wanted: WireAnchor | undefined) =>
				live !== undefined
					? wanted?.live?.min === live.min
					: wanted?.joined !== undefined &&
						wanted.joined.absolute === joined?.absolute &&
						wanted.joined.frame === joined.frame;
			const index = adding.findIndex(
				([range, kind]) => kind === kindOf(id) && same(range.start) && rangeKey({ end: range.end }) === end,
			);
			if (index < 0) return true;
			retuned.push([id, adding[index][0]]);
			adding.splice(index, 1);
			return false;
		});
		const add: WireRange[] = [];
		const fillIds: bigint[] = [];
		const probeIds: bigint[] = [];
		const first = this.#next;
		for (const [range, kind] of adding) {
			add.push(range);
			const id = first + BigInt(add.length - 1);
			if (kind === "fill") fillIds.push(id);
			else if (kind === "probe") probeIds.push(id);
		}
		// Checked before anything changes, so a refused edit leaves the ranges as last sent.
		for (const range of add) size += rangeSize(range);
		// A retuned join was counted out among the removed, so it counts back in as it now reads.
		for (const [, range] of retuned) size += rangeSize(range);
		if (size > Message.MAX_MESSAGE_SIZE) throw new Error("too many ranges for one SUBSCRIBE");
		for (const id of kept) this.#remove(id);
		for (const [id, range] of retuned) this.#replace(id, range);
		this.#add(add);
		const retune = retuned.map(([id, range]): [bigint, number | null] => [
			id,
			range.start?.live?.max ?? range.start?.joined?.max ?? null,
		]);
		return { remove: kept, add, retune, fillIds, probeIds };
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

/**
 * Decode SUBSCRIBE's trailing `Meta` byte, lite-07's only: 1 for headers alone, 0 for the payload,
 * anything else malformed. An earlier wire always asks for the payload.
 */
async function decodeMeta(r: Reader, version: Version): Promise<boolean> {
	if (!hasMeta(version)) return false;
	const meta = await r.u8();
	switch (meta) {
		case 0:
			return false;
		case 1:
			return true;
		default:
			throw new ProtocolViolation(`unknown meta: ${meta}`);
	}
}

/** The inverse of {@link decodeMeta}, throwing for a metadata subscription on a wire without one. */
async function encodeMeta(w: Writer, version: Version, meta: boolean) {
	if (hasMeta(version)) {
		await w.u8(meta ? 1 : 0);
	} else if (meta) {
		throw new Error("a metadata subscription needs moq-lite-07");
	}
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

/** What lite-03 to lite-06 send for no `Subscriber Max Age` limit, which their bare milliseconds can't say. */
const MAX_SAFE_DELAY_MS = Number.MAX_SAFE_INTEGER;

/**
 * Encode the `Subscriber Max Age` before lite-07, bare milliseconds with `null` (no limit) as
 * {@link MAX_SAFE_DELAY_MS}. Lite-07 carries none: each live start has its own.
 */
async function encodeMaxDelay(w: Writer, version: Version, maxDelay: number | null) {
	if (hasRanges(version)) return;
	await w.u53(maxDelay ?? MAX_SAFE_DELAY_MS);
}

/** Decode the inverse of {@link encodeMaxDelay}: {@link MAX_SAFE_DELAY_MS} or more says no limit. */
async function decodeMaxDelay(r: Reader, version: Version): Promise<number | null> {
	if (hasRanges(version)) return null;
	// Read wide: a peer may send any varint at or past the sentinel, which still means no limit.
	const millis = await r.u62();
	return millis < BigInt(MAX_SAFE_DELAY_MS) ? Number(millis) : null;
}

export class SubscribeUpdate {
	/** A refresh request for this group or later, judged against the local track's refresh. */
	wantedGroup?: bigint;
	priority: number;
	/** See {@link Subscribe.order}. */
	order: Order;
	/** See {@link Subscribe.maxDelay}. */
	maxDelay: number | null;
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
	/**
	 * Lite-07: each live join to give a new max in milliseconds (`null` for no limit) in place,
	 * keeping where it was placed, by Range ID.
	 */
	retune?: [bigint, number | null][];

	constructor(props: {
		priority: number;
		wantedGroup?: bigint;
		order?: Order;
		maxDelay?: number | null;
		startGroup?: bigint;
		endGroup?: bigint;
		startFrame?: number;
		endFrame?: number;
		ranges?: WireRange[];
		remove?: bigint[];
		retune?: [bigint, number | null][];
	}) {
		if (props.wantedGroup !== undefined && props.wantedGroup < 0n)
			throw new RangeError("wanted group must be non-negative");
		this.wantedGroup = props.wantedGroup;
		this.priority = props.priority;
		this.order = props.order ?? "descending";
		this.maxDelay = props.maxDelay === undefined ? 0 : props.maxDelay;
		this.startGroup = props.startGroup;
		this.endGroup = props.endGroup;
		this.startFrame = props.startFrame ?? 0;
		this.endFrame = props.endFrame;
		this.ranges = props.ranges;
		this.remove = props.remove;
		this.retune = props.retune;
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
					const retune = this.retune ?? [];
					await w.u53(retune.length);
					for (const [id, max] of retune) {
						await w.u62(id);
						await Duration.encodeOptional(w, max ?? undefined);
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
				let span: Span & { ranges?: WireRange[]; remove?: bigint[]; retune?: [bigint, number | null][] };
				if (hasRanges(version)) {
					// Not preallocated, like the ranges: each id costs a byte of the bounded message.
					const remove: bigint[] = [];
					for (let count = await r.u53(); count > 0; count--) remove.push(await r.u62());
					const ranges = await decodeRanges(r);
					const retune: [bigint, number | null][] = [];
					for (let count = await r.u53(); count > 0; count--)
						retune.push([await r.u62(), (await Duration.decodeOptional(r)) ?? null]);
					span = { startFrame: 0, remove, ranges, retune };
				} else {
					span = await decodeSpan(r, version);
				}
				const wanted = hasRefresh(version) ? await r.u62() : 0n;
				return new SubscribeUpdate({
					wantedGroup: wanted === 0n ? undefined : wanted - 1n,
					priority,
					order,
					maxDelay,
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
	/** The publisher identity the subscriber expects. Lite-07+. */
	identity?: Identity.Valid;
	track: string;
	priority: number;
	/**
	 * Which pending group to send first. Lite-07+; earlier wires decode as descending and
	 * cannot ask for anything else.
	 */
	order: Order;
	/**
	 * The `Subscriber Max Age` before lite-07, in milliseconds, `null` for no limit beyond the
	 * publisher's; zero skips once a newer group is available. Lite-01/02 carry none and decode as
	 * `null`. Lite-07 carries a max on each live start instead, so this is ignored there and decodes
	 * as `null`.
	 */
	maxDelay: number | null;

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

	/**
	 * Whether each frame is delivered as its header alone, without its payload, for the
	 * subscription's whole life. Lite-07+: an earlier wire cannot carry it, so encoding one there
	 * throws rather than asking for the payload.
	 */
	meta: boolean;

	constructor(props: {
		id: bigint;
		broadcast: Path.Valid;
		identity?: Identity.Valid;
		track: string;
		priority: number;
		order?: Order;
		maxDelay?: number | null;
		startGroup?: bigint;
		endGroup?: bigint;
		startFrame?: number;
		endFrame?: number;
		ranges?: WireRange[];
		meta?: boolean;
	}) {
		this.id = props.id;
		this.broadcast = props.broadcast;
		this.identity = props.identity;
		this.track = props.track;
		this.priority = props.priority;
		this.order = props.order ?? "descending";
		this.maxDelay = props.maxDelay === undefined ? 0 : props.maxDelay;
		this.startGroup = props.startGroup;
		this.endGroup = props.endGroup;
		this.startFrame = props.startFrame ?? 0;
		this.endFrame = props.endFrame;
		this.ranges = props.ranges;
		this.meta = props.meta ?? false;
	}

	async #encode(w: Writer, version: Version) {
		await w.u62(this.id);
		await w.string(Path.encode(this.broadcast));
		await encodePin(w, version, this.identity);
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
				await encodeSpan(w, version, this, this.ranges);
				break;
		}
		await encodeMeta(w, version, this.meta);
	}

	static async #decode(r: Reader, version: Version): Promise<Subscribe> {
		const id = await r.u62();
		const broadcast = Path.decode(await r.string());
		const identity = await decodePin(r, version);
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
				const span = await decodeSpan(r, version);
				const meta = await decodeMeta(r, version);
				return new Subscribe({
					id,
					broadcast,
					identity,
					track,
					priority,
					order,
					maxDelay,
					...span,
					meta,
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

/** The publisher's largest position, as SUBSCRIBE_START reports it. Mirrors the Rust `Largest`. */
export interface Largest {
	/** The newest group and its last frame so far. */
	position: Position;
	/** The group ended at that frame, so no frame follows it: a reader knows at once that it isn't the live one. */
	ended: boolean;
}

/**
 * What a SUBSCRIBE_START knows of the track's largest position: `"unknown"` when the answering
 * hop has yet to learn it, `"empty"` for a track with no group yet, or the {@link Largest}.
 * Mirrors the Rust `Edge`.
 */
export type Edge = "unknown" | "empty" | Largest;

/**
 * Says where one range of a Draft-05+ subscription starts, once the publisher resolves it. Before
 * lite-07 one answers the subscription's one range; lite-07 answers each range by Range ID, from
 * the SUBSCRIBE or an update, along with what the publisher knows of the largest position. A start
 * above the requested one implicitly drops the groups below it.
 *
 * There is no start frame: a partial group is only served to a subscriber that asked for
 * one, so a range begins at its requested frame when it starts at its requested group,
 * and at frame 0 of a later one otherwise.
 */
export class SubscribeStart {
	/** The range's Range ID; 0 before lite-07, whose one range has none on the wire. */
	id: bigint;
	/** The group its delivery starts at, `undefined` for a range with no group there. */
	start?: bigint;
	/** What the publisher knows of the largest position; always `"unknown"` before lite-07. */
	largest: Edge;

	constructor(props: { id: bigint; start?: bigint; largest: Edge }) {
		this.id = props.id;
		this.start = props.start;
		this.largest = props.largest;
	}

	async encode(w: Writer, version: Version): Promise<void> {
		return Message.encode(w, async (w) => {
			if (!hasRanges(version)) {
				if (this.id !== 0n || this.start === undefined || this.largest !== "unknown")
					throw new Error("one start group before draft-07");
				return w.u62(this.start);
			}
			await w.u62(this.id);
			// Group + 1, so 0 is a range with no group there.
			await w.u62(this.start === undefined ? 0n : this.start + 1n);
			if (this.largest === "unknown") return w.u8(0);
			if (this.largest === "empty") return w.u8(1);
			await w.u8(2);
			await w.u62(this.largest.position.group);
			await w.u53(this.largest.position.frame);
			await w.u8(this.largest.ended ? 1 : 0);
		});
	}

	static async decode(r: Reader, version: Version): Promise<SubscribeStart> {
		return Message.decode(r, async (r) => {
			if (!hasRanges(version)) return new SubscribeStart({ id: 0n, start: await r.u62(), largest: "unknown" });
			const id = await r.u62();
			const wire = await r.u62();
			const start = wire === 0n ? undefined : wire - 1n;
			const type = await r.u8();
			switch (type) {
				case 0:
					return new SubscribeStart({ id, start, largest: "unknown" });
				case 1:
					return new SubscribeStart({ id, start, largest: "empty" });
				case 2: {
					const position = { group: await r.u62(), frame: await r.u53() };
					const ended = await r.u8();
					if (ended > 1) throw new ProtocolViolation(`invalid Largest Ended: ${ended}`);
					return new SubscribeStart({ id, start, largest: { position, ended: ended === 1 } });
				}
				default:
					throw new ProtocolViolation(`invalid Largest Type: ${type}`);
			}
		});
	}
}

/**
 * SUBSCRIBE_END: no group at or after `group` (exclusive upper bound) will be produced on a
 * Draft-05+ subscription. `0` means the track ended before producing any groups.
 */
export class SubscribeEnd {
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

	static async decode(r: Reader, version: Version): Promise<SubscribeEnd> {
		return Message.decode(r, async (r) => {
			const group = await r.u62();
			if (!hasEndError(version)) return new SubscribeEnd(group);
			const code = await r.u53();
			return new SubscribeEnd(group, code === 0 ? undefined : code - 1);
		});
	}
}

/**
 * A SUBSCRIBE_DROP range: a non-empty `[Group Start, Group End)` on lite-07, an
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
 * Names groups that will not be delivered: aborted or reset, evicted or expired with no copy
 * left, stale, or (on lite-07+ with {@link StreamCode.Skip}) starts declared never produced.
 *
 * Draft-03+; publishers send it on Draft-05 and later.
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
 * A response message on the subscribe stream, prefixed with a type discriminator
 * on Draft-03+.
 *
 * The discriminator is version-dependent:
 * - Draft-03/04: `0x0` SUBSCRIBE_OK, `0x1` SUBSCRIBE_DROP.
 * - Draft-05/06: `0x0` SUBSCRIBE_START, `0x1` SUBSCRIBE_END, `0x2` SUBSCRIBE_DROP (SUBSCRIBE_OK
 *   was removed; acceptance is implicit).
 * - Lite-07+: `0x1` SUBSCRIBE_END, `0x2` SUBSCRIBE_DROP (with `SKIP` for declared skips), and
 *   `0x4` SUBSCRIBE_START, one per range; `0x0` and `0x3` are unused.
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
				await resp.drop.encode(w, version);
			} else {
				throw new Error("SUBSCRIBE_START/FINISH not supported for this version");
			}
			break;
		default:
			// Draft-05+: SUBSCRIBE_OK is gone; START/END/DROP carry the resolved range.
			if ("start" in resp) {
				await w.u53(hasRanges(version) ? 0x4 : 0x0);
				await resp.start.encode(w, version);
			} else if ("end" in resp) {
				await w.u53(0x1);
				await resp.end.encode(w, version);
			} else if ("drop" in resp) {
				await w.u53(0x2);
				await resp.drop.encode(w, version);
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
					if (hasRanges(version)) throw new ProtocolViolation(`unknown subscribe response type: ${typ}`);
					return { start: await SubscribeStart.decode(r, version) };
				case 0x1:
					return { end: await SubscribeEnd.decode(r, version) };
				case 0x2:
					return { drop: await SubscribeDrop.decode(r, version) };
				case 0x4:
					if (!hasRanges(version)) throw new ProtocolViolation(`unknown subscribe response type: ${typ}`);
					return { start: await SubscribeStart.decode(r, version) };
				default:
					throw new ProtocolViolation(`unknown subscribe response type: ${typ}`);
			}
		}
	}
}

/**
 * Whether the next response `r` holds whole, without reading the stream, is a lite-07
 * SUBSCRIBE_START, so a subscriber applying a burst of them can take the next before applying.
 */
export function startBuffered(r: Reader, version: Version): boolean {
	if (!hasRanges(version)) return false;
	const start = r.tryPeek((c) => {
		if (c.u53() !== 0x4) return false;
		c.read(c.u53());
		return true;
	});
	return start === true;
}

/** Like {@link decodeSubscribeResponse} but resolves `undefined` on a clean FIN. */
export async function decodeSubscribeResponseMaybe(
	r: Reader,
	version: Version,
): Promise<SubscribeResponse | undefined> {
	if (await r.done()) return undefined;
	return decodeSubscribeResponse(r, version);
}

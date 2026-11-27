import { expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import { ProtocolViolation, StreamCode, StreamError } from "../error.ts";
import { NO_MAX_DELAY } from "../internal.ts";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import type { Range } from "../track.ts";
import { rangesOf, wireSpan } from "../window.ts";
import { MAX_MESSAGE_SIZE } from "./message.ts";
import {
	decodeSubscribeResponse,
	encodeSubscribeResponse,
	groupPolicy,
	inclusiveGroupEnd,
	LiveRanges,
	legacyStart,
	type Policy,
	positionFrames,
	positionStops,
	type Retune,
	rangeBounds,
	rangeSize,
	requestedPlaced,
	requestedRange,
	requestedRanges,
	Subscribe,
	SubscribeDrop,
	SubscribeEnd,
	SubscribeOk,
	type SubscribeResponse,
	SubscribeStart,
	SubscribeUpdate,
	spanOf,
	subscriptionPolicy,
	toWireRanges,
	type WireRange,
	withPolicy,
} from "./subscribe.ts";

/** A retune of a live join's max alone, at the default policy. */
const max = (max: number | null): Retune => ({ priority: 0, order: "descending", max });

import { Version } from "./version.ts";

function concat(chunks: Uint8Array[]): Uint8Array {
	const total = chunks.reduce((sum, c) => sum + c.byteLength, 0);
	const out = new Uint8Array(total);
	let offset = 0;
	for (const c of chunks) {
		out.set(c, offset);
		offset += c.byteLength;
	}
	return out;
}

async function encode(version: Version, resp: SubscribeResponse): Promise<Uint8Array> {
	const written: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void written.push(new Uint8Array(chunk)) }),
		version,
	);
	await encodeSubscribeResponse(writer, resp, version);
	writer.close();
	await writer.closed;
	return concat(written);
}

async function responseRoundtrip(version: Version, resp: SubscribeResponse): Promise<SubscribeResponse> {
	const reader = new Reader(undefined, await encode(version, resp), version);
	return decodeSubscribeResponse(reader, version);
}

async function encodeSubscribe(msg: Subscribe): Promise<void> {
	const writer = new Writer(new WritableStream<Uint8Array>(), Version.DRAFT_06);
	try {
		await msg.encode(writer, Version.DRAFT_06);
	} finally {
		writer.close();
	}
}

async function encodeMessage(
	version: Version,
	message: { encode(writer: Writer, version: Version): Promise<void> },
): Promise<Uint8Array> {
	const written: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void written.push(new Uint8Array(chunk)) }),
		version,
	);
	await message.encode(writer, version);
	writer.close();
	await writer.closed;
	return concat(written);
}

test("SubscribeOk round-trips priority/groups on draft-04", async () => {
	const got = await responseRoundtrip(Version.DRAFT_04, {
		ok: new SubscribeOk({ priority: 7, maxDelay: 250, startGroup: 3n }),
	});
	expect("ok" in got).toBe(true);
	if (!("ok" in got)) throw new Error("expected ok");
	expect(got.ok.priority).toBe(7);
	expect(got.ok.startGroup).toBe(3n);
});

test("Subscribe round-trips every option including startGroup 0", async () => {
	const message = new Subscribe({
		id: 4n,
		broadcast: Path.from("test"),
		track: "video",
		priority: 7,
		maxDelay: 250,
		startGroup: 0n,
		endGroup: 9n,
	});
	// Lite-06 carries the raw floor, and a floor of 0 with no frame offset is the same
	// absence of a constraint as no floor at all, so it canonicalizes to undefined.
	const got = await Subscribe.decode(
		new Reader(undefined, await encodeMessage(Version.DRAFT_06, message), Version.DRAFT_06),
		Version.DRAFT_06,
	);
	expect(got.priority).toBe(7);
	expect(got.maxDelay).toBe(250);
	expect(got.startGroup).toBeUndefined();
	expect(got.endGroup).toBe(9n);

	// Group 0 stays named when a Frame Start qualifies it: a subscription can resume
	// partway through group 0 (a catalog never leaves it).
	message.startFrame = 4;
	const resumed = await Subscribe.decode(
		new Reader(undefined, await encodeMessage(Version.DRAFT_06, message), Version.DRAFT_06),
		Version.DRAFT_06,
	);
	expect(resumed.startGroup).toBe(0n);
	expect(resumed.startFrame).toBe(4);
	message.startFrame = 0;

	// A pre-06 wire encodes an explicit group 0 as sequence + 1, so it round-trips.
	// Omitting the floor is still the latest group, not group 0.
	const explicit = await Subscribe.decode(
		new Reader(undefined, await encodeMessage(Version.DRAFT_05, message), Version.DRAFT_05),
		Version.DRAFT_05,
	);
	expect(explicit.startGroup).toBe(0n);
	expect(explicit.endGroup).toBe(9n);
	const absent = await Subscribe.decode(
		new Reader(
			undefined,
			await encodeMessage(Version.DRAFT_05, new Subscribe({ ...message, startGroup: undefined })),
			Version.DRAFT_05,
		),
		Version.DRAFT_05,
	);
	expect(absent.startGroup).toBeUndefined();
});

test("SubscribeUpdate round-trips every option including startGroup 0", async () => {
	const message = new SubscribeUpdate({
		priority: 8,
		maxDelay: 500,
		startGroup: 0n,
		endGroup: 12n,
	});
	const got = await SubscribeUpdate.decode(
		new Reader(undefined, await encodeMessage(Version.DRAFT_06, message), Version.DRAFT_06),
		Version.DRAFT_06,
	);
	expect(got.priority).toBe(8);
	expect(got.maxDelay).toBe(500);
	expect(got.startGroup).toBeUndefined();
	expect(got.endGroup).toBe(12n);

	// The same pre-06 encoding as SUBSCRIBE: explicit group 0 round-trips, and an
	// omitted floor stays omitted.
	const explicit = await SubscribeUpdate.decode(
		new Reader(undefined, await encodeMessage(Version.DRAFT_05, message), Version.DRAFT_05),
		Version.DRAFT_05,
	);
	expect(explicit.startGroup).toBe(0n);
	expect(explicit.endGroup).toBe(12n);
	const absent = await SubscribeUpdate.decode(
		new Reader(
			undefined,
			await encodeMessage(Version.DRAFT_05, new SubscribeUpdate({ ...message, startGroup: undefined })),
			Version.DRAFT_05,
		),
		Version.DRAFT_05,
	);
	expect(absent.startGroup).toBeUndefined();
});

test("lite-07 carries the span as its only range, exclusive at the end", async () => {
	const message = new Subscribe({
		id: 1n,
		broadcast: Path.from("test"),
		track: "video",
		priority: 0,
		startGroup: 3n,
		startFrame: 2,
		endGroup: 9n,
		endFrame: 4,
	});
	const decode = async (msg: Subscribe) =>
		Subscribe.decode(
			new Reader(undefined, await encodeMessage(Version.DRAFT_07, msg), Version.DRAFT_07),
			Version.DRAFT_07,
		);
	// A range ends only at a group: a slice of one is a FETCH.
	await expect(encodeMessage(Version.DRAFT_07, message)).rejects.toThrow("frame bounds not supported");
	await expect(encodeMessage(Version.DRAFT_07, new Subscribe({ ...message, startFrame: 0 }))).rejects.toThrow(
		"frame bounds not supported",
	);
	// It starts partway through one as a Position.
	const resumed = await decode(new Subscribe({ ...message, endFrame: undefined }));
	expect([resumed.startGroup, resumed.startFrame, resumed.ranges]).toEqual([
		3n,
		2,
		[{ start: { absolute: 3n, frame: 2 }, end: { time: 9n } }],
	]);
	const whole = await decode(new Subscribe({ ...message, startFrame: 0, endFrame: undefined }));
	expect([whole.startGroup, whole.startFrame, whole.endGroup, whole.endFrame]).toEqual([3n, 0, 9n, undefined]);
	const open = await decode(
		new Subscribe({ ...message, startGroup: undefined, startFrame: 0, endGroup: undefined, endFrame: undefined }),
	);
	expect([open.startGroup, open.endGroup]).toEqual([undefined, undefined]);
});

test("a lite-07 list decodes its time anchors and starts with none", async () => {
	// No Range ID to retire, the ranges it adds, each at priority 0 and descending (`P`), no range
	// to retune, and an absent wanted group.
	const P = [0x80, 0];
	const update = async (ranges: number[]) => {
		const body = [0, ...ranges, 0, 0];
		const got = await SubscribeUpdate.decode(
			new Reader(undefined, new Uint8Array([body.length, ...body]), Version.DRAFT_07),
			Version.DRAFT_07,
		);
		return { ...got, ...spanOf(Version.DRAFT_07, got.ranges ?? []) };
	};

	// Type 1 is a time, both ends inclusive, and type 0 none. A time names the group it keys, so
	// the span covers it, and the model reads it as the same time, the end inclusive.
	const timed = await update([2, ...P, 1, 40, 1, 90, ...P, 0, 0]);
	expect(timed.ranges).toEqual([{ start: { time: 40n }, end: { time: 90n } }, {}]);
	expect([timed.startGroup, timed.endGroup]).toEqual([undefined, undefined]);
	expect(requestedRanges(timed)).toEqual([{ start: 40n, end: 90n }, {}]);
	const bounded = await update([1, ...P, 1, 40, 1, 90]);
	expect([bounded.startGroup, bounded.endGroup]).toEqual([40n, 90n]);

	// One ending before its start covers no group there, and keeps its place.
	const reversed = await update([1, ...P, 1, 5, 1, 3]);
	expect(requestedRanges(reversed)).toEqual([{ start: 5n, end: 3n }]);

	// A time past what a JS number holds exactly reads at full width.
	const far = 1n << 60n;
	const wide = new SubscribeUpdate({ priority: 0, ranges: [{ start: { time: far } }] });
	const got = await SubscribeUpdate.decode(
		new Reader(undefined, await encodeMessage(Version.DRAFT_07, wide), Version.DRAFT_07),
		Version.DRAFT_07,
	);
	expect(got.ranges).toEqual([{ start: { time: far } }]);
	expect(requestedRanges({ ...got, startFrame: 0 })).toEqual([{ start: far }]);

	// An update may add no range, retiring some or none.
	expect((await update([0])).ranges).toEqual([]);
	// Type 3 is a live join's lag then its max, each plus one, 0 for none, which only a start may carry.
	expect((await update([1, ...P, 3, 8, 0, 0])).ranges).toEqual([{ start: { live: { min: 7, max: null } } }]);
	expect((await update([1, ...P, 3, 8, 5, 0])).ranges).toEqual([{ start: { live: { min: 7, max: 4 } } }]);
	expect((await update([1, ...P, 3, 0, 5, 0])).ranges).toEqual([{ start: { live: { min: null, max: 4 } } }]);
	await expect(update([1, ...P, 0, 3, 7, 0])).rejects.toThrow(ProtocolViolation);
	// Type 4 is a placed join's group, a frame that may be 0, then its max as type 3 carries it,
	// which only a start may carry either.
	expect((await update([1, ...P, 4, 9, 0, 5, 0])).ranges).toEqual([
		{ start: { joined: { absolute: 9n, frame: 0, max: 4 } } },
	]);
	await expect(update([1, ...P, 0, 4, 9, 0, 0])).rejects.toThrow(ProtocolViolation);
	// Position at frame 0 is valid as both start and end (exact group match / exclusive group end).
	expect((await update([1, ...P, 2, 0, 0, 0])).ranges).toEqual([{ start: { absolute: 0n, frame: 0 } }]);
	expect((await update([1, ...P, 1, 0, 2, 7, 0])).ranges).toEqual([
		{ start: { time: 0n }, end: { absolute: 7n, frame: 0 } },
	]);
	expect((await update([1, ...P, 1, 0, 2, 7, 4])).ranges).toEqual([
		{ start: { time: 0n }, end: { absolute: 7n, frame: 4 } },
	]);
	// Any other type is a violation.
	await expect(update([1, ...P, 5, 0, 0])).rejects.toThrow(ProtocolViolation);
	await expect(update([1, ...P, 0, 5, 0])).rejects.toThrow(ProtocolViolation);
});

// A placed live join rides as type 4: its group, a frame that may be 0, then its max age as a live
// start carries it. Only a start, and only lite-07: an older wire refuses it for the caller to carry
// as a start first. Mirrors the Rust `a_joined_start_roundtrips_on_lite07`.
test("a joined start round-trips on lite-07", async () => {
	const version = Version.DRAFT_07;
	const joined = (absolute: bigint, frame: number, max: number | null): WireRange => ({
		start: { joined: { absolute, frame, max } },
		end: { time: 9_000n },
	});
	const props = { id: 1n, broadcast: Path.from("room"), track: "video", priority: 3 };
	for (const range of [joined(1_500n, 0, null), joined(1_500n, 3, 0), joined(0n, 7, 250)]) {
		const subscribe = await encodeMessage(version, new Subscribe({ ...props, ranges: [range] }));
		expect((await Subscribe.decode(new Reader(undefined, subscribe, version), version)).ranges).toEqual([range]);
	}
	// The model rounds its max up to whole milliseconds, as a live start's.
	const join: Range = { start: { live: { min: Duration.millis(40), max: Duration.micros(1500) } } };
	expect(toWireRanges([join, join], new Map([[1, 9n]]))).toEqual([
		{ start: { live: { min: 40, max: 2 } } },
		{ start: { joined: { absolute: 9n, frame: 0, max: 2 } } },
	]);

	// The count, the range's priority and order, the type, group, frame, and max age plus one, then
	// the end's type and time, 251 and 9000 each in two bytes.
	const buf = await encodeMessage(version, new Subscribe({ ...props, ranges: [joined(5n, 0, 250)] }));
	expect([...buf.slice(buf.length - 11)]).toEqual([1, 0x80, 0, 4, 5, 0, 0x80, 251, 1, 0xa3, 0x28]);

	// An end can't be a join, placed or not.
	await expect(
		encodeMessage(version, new Subscribe({ ...props, ranges: [{ end: joined(5n, 0, null).start }] })),
	).rejects.toThrow();
});

// A placed join decodes as a live join with no lag, placed at its group, its frame a Position's.
// Mirrors the Rust `a_joined_start_is_a_placed_live_join`.
test("a joined start is a placed live join", () => {
	const joined = (frame: number): WireRange[] => [{ start: { joined: { absolute: 5n, frame, max: 1_000 } } }];
	expect(requestedRanges({ startFrame: 0, ranges: joined(3) })).toEqual([
		{ start: { live: { min: Duration.ZERO, max: Duration.secs(1) } } },
	]);
	expect(requestedPlaced({ startFrame: 0, ranges: joined(3) })).toEqual(new Map([[0, 5n]]));
	expect(rangeBounds(joined(3)[0])).toEqual([5n, undefined]);
	expect(positionFrames(joined(3))).toEqual(new Map([[5n, 3]]));
	// Frame 0 is the whole group.
	expect(positionFrames(joined(0))).toEqual(new Map());
});

// Before lite-07 one max age bounds a named start too, so it decodes as lite-07's Joined anchor
// does. Mirrors the Rust `a_legacy_start_with_a_max_age_is_a_placed_join`.
test("a legacy start with a max age is a placed join", () => {
	const legacy = legacyStart(Version.DRAFT_06, 2_000);
	const named = { startGroup: 5n, startFrame: 0 };
	const joined = { startFrame: 0, ranges: [{ start: { joined: { absolute: 5n, frame: 0, max: 2_000 } } }] };
	expect(requestedRanges(named, legacy)).toEqual(requestedRanges(joined));
	expect(requestedPlaced(named, legacy)).toEqual(requestedPlaced(joined));
	// With no max age, it is history.
	const history = legacyStart(Version.DRAFT_06, null);
	expect(requestedRanges(named, history)).toEqual([{ start: 5n }]);
	expect(requestedPlaced(named, history)).toEqual(new Map());
});

test("a lite-07 range may start partway through a group, which the span keeps", async () => {
	const position = { start: { absolute: 7n, frame: 4 } };
	const update = new SubscribeUpdate({ priority: 0, ranges: [position] });
	const bytes = await encodeMessage(Version.DRAFT_07, update);
	// One range: its priority and order, type 2, start 7, frame 4, then no end, no range to
	// retune, and an absent wanted group.
	expect(Array.from(bytes.slice(-9))).toEqual([1, 0x80, 0, 2, 7, 4, 0, 0, 0]);
	expect(rangeSize(position)).toBe(6);
	const got = await SubscribeUpdate.decode(new Reader(undefined, bytes, Version.DRAFT_07), Version.DRAFT_07);
	expect(got.ranges).toEqual([position]);
	const span = spanOf(Version.DRAFT_07, got.ranges ?? []);
	expect([span.startGroup, span.startFrame]).toEqual([7n, 4]);
	// Group 0 stays named when a frame qualifies it.
	const zero = spanOf(Version.DRAFT_07, [{ start: { absolute: 0n, frame: 2 } }]);
	expect([zero.startGroup, zero.startFrame]).toEqual([0n, 2]);
	// The model reads the group whole: the publisher cuts the frames.
	expect(requestedRanges(span)).toEqual([{ start: 7n }]);

	// Another range that may cover the group asks for it whole, since a group is delivered once.
	for (const other of [{ start: { time: 3n } }, {}, { end: { time: 9n } }]) {
		expect(spanOf(Version.DRAFT_07, [position, other]).startFrame).toBe(0);
	}
	expect(spanOf(Version.DRAFT_07, [position, { start: { time: 9n } }]).startFrame).toBe(4);
	// The span qualifies only its lowest start; the publisher reads every Position from the ranges.
	expect(spanOf(Version.DRAFT_07, [position, { start: { time: 1n }, end: { time: 3n } }]).startFrame).toBe(0);
	const disjoint = [{ start: { absolute: 7n, frame: 4 }, end: { time: 8n } }, { start: { absolute: 9n, frame: 2 } }];
	expect([...positionFrames(disjoint)]).toEqual([
		[7n, 4],
		[9n, 2],
	]);
	const overlapping = [{ start: { absolute: 7n, frame: 4 } }, { start: { absolute: 9n, frame: 2 } }];
	expect([...positionFrames(overlapping)]).toEqual([[7n, 4]]);
	// A Position range ending before its group covers nothing, so the group goes whole.
	expect([...positionFrames([{ start: { absolute: 7n, frame: 4 }, end: { time: 6n } }, {}])]).toEqual([]);

	// The sorted sweep keeps exactly what a scan of every range per Position keeps.
	let seed = 0x6672616d;
	const random = (n: number) => {
		seed = (seed * 1103515245 + 12345) % 2 ** 31;
		return seed % n;
	};
	for (let i = 0; i < 2000; i++) {
		const anchor = () => (random(3) === 0 ? undefined : { time: BigInt(random(8)) });
		const ranges: WireRange[] = [];
		for (let n = random(6); n > 0; n--) {
			const range: WireRange =
				random(3) === 0
					? { start: { absolute: BigInt(random(8)), frame: 1 + random(3) } }
					: { start: anchor() };
			if (random(5) < 3) range.end = { time: BigInt(random(8)) };
			ranges.push(range);
		}
		const scanned = new Map<bigint, number>();
		for (const range of ranges) {
			const start = range.start;
			if (start?.absolute === undefined || !start.frame) continue;
			const own = range.end?.absolute ?? (range.end?.time === undefined ? undefined : range.end.time + 1n);
			if (own !== undefined && own <= start.absolute) continue;
			let covering = 0;
			for (const other of ranges) {
				const from = other.start?.absolute ?? other.start?.time;
				const to = other.end?.absolute ?? (other.end?.time === undefined ? undefined : other.end.time + 1n);
				if ((from !== undefined && from > start.absolute) || (to !== undefined && to <= start.absolute))
					continue;
				covering++;
			}
			if (covering === 1) scanned.set(start.absolute, start.frame);
		}
		expect(positionFrames(ranges)).toEqual(scanned);
	}
});

test("a lite-07 range may end partway through a group, stopping it there", async () => {
	const position = { start: { time: 3n }, end: { absolute: 9n, frame: 2 } };
	const update = new SubscribeUpdate({ priority: 0, ranges: [position] });
	const bytes = await encodeMessage(Version.DRAFT_07, update);
	// Type 1, time 3, then type 2, start 9, frame 2, which it stops before, no live join to retune,
	// and an absent wanted group.
	expect(Array.from(bytes.slice(-7))).toEqual([1, 3, 2, 9, 2, 0, 0]);
	const got = await SubscribeUpdate.decode(new Reader(undefined, bytes, Version.DRAFT_07), Version.DRAFT_07);
	expect(got.ranges).toEqual([position]);
	// The model range includes group 9, and only the frames before frame 2 of it go out.
	expect(requestedRange(position)).toEqual({ start: 3n, end: 9n });
	expect(positionStops([position])).toEqual(new Map([[9n, 2]]));
	// Another range covering group 9 asks for it whole, and one starting past it stops nothing.
	expect(positionStops([position, { start: { time: 9n } }])).toEqual(new Map());
	expect(positionStops([{ start: { time: 10n }, end: { absolute: 9n, frame: 2 } }])).toEqual(new Map());
	// One stopping where it starts holds nothing.
	expect(requestedRange({ start: { absolute: 9n, frame: 2 }, end: { absolute: 9n, frame: 2 } })).toEqual({
		start: 1n,
		end: 0n,
	});
	expect(requestedRange({ start: { absolute: 9n, frame: 1 }, end: { absolute: 9n, frame: 2 } })).toEqual({
		start: 9n,
		end: 9n,
	});
	// It covers no group either, so it neither stops nor starts one, nor keeps another range's
	// stop or start from applying.
	const empty = { start: { absolute: 9n, frame: 2 }, end: { absolute: 9n, frame: 2 } };
	expect(positionStops([empty])).toEqual(new Map());
	expect(positionFrames([empty])).toEqual(new Map());
	expect(positionStops([empty, position])).toEqual(new Map([[9n, 2]]));
	expect(positionFrames([empty, { start: { absolute: 9n, frame: 3 } }])).toEqual(new Map([[9n, 3]]));
});

test("lite-07 sends an absolute anchor as a Position at frame 0", async () => {
	const update = new SubscribeUpdate({
		priority: 0,
		ranges: [{ start: { absolute: 7n }, end: { absolute: 10n } }],
	});
	const got = await SubscribeUpdate.decode(
		new Reader(undefined, await encodeMessage(Version.DRAFT_07, update), Version.DRAFT_07),
		Version.DRAFT_07,
	);
	expect(got.ranges).toEqual([{ start: { absolute: 7n, frame: 0 }, end: { absolute: 10n, frame: 0 } }]);
	// Position(0, 0) as an exclusive end encodes without underflow.
	const zero = await SubscribeUpdate.decode(
		new Reader(
			undefined,
			await encodeMessage(
				Version.DRAFT_07,
				new SubscribeUpdate({ priority: 0, ranges: [{ end: { absolute: 0n } }] }),
			),
			Version.DRAFT_07,
		),
		Version.DRAFT_07,
	);
	expect(zero.ranges).toEqual([{ end: { absolute: 0n, frame: 0 } }]);
});

test("lite-07 carries every range, times and no start included, and older wires the span alone", async () => {
	// A model time is already a group start in the track's timescale: it goes out as is.
	const ranges = toWireRanges([{ start: 2n, end: 4n }, {}, { end: 3n }, { start: 1_500n, end: 3_000n }]);
	expect(ranges[3]).toEqual({ start: { time: 1_500n }, end: { time: 3_000n } });
	const message = new Subscribe({
		id: 1n,
		broadcast: Path.from("test"),
		track: "video",
		priority: 0,
		startGroup: 0n,
		ranges,
	});
	const decode = async (version: Version) =>
		Subscribe.decode(new Reader(undefined, await encodeMessage(version, message), version), version);
	expect((await decode(Version.DRAFT_07)).ranges).toEqual(ranges);
	const older = await decode(Version.DRAFT_05);
	expect([older.ranges, older.startGroup, older.endGroup]).toEqual([undefined, 0n, undefined]);
	await expect(encodeMessage(Version.DRAFT_07, new Subscribe({ ...message, ranges: [] }))).rejects.toThrow(
		"empty subscription range cannot be encoded",
	);
});

test("SubscribeStart round-trips on draft-05", async () => {
	const got = await responseRoundtrip(Version.DRAFT_05, {
		start: new SubscribeStart({ id: 0n, start: 42n, largest: "unknown" }),
	});
	if (!("start" in got)) throw new Error("expected start");
	expect([got.start.id, got.start.start, got.start.largest]).toEqual([0n, 42n, "unknown"]);
	// Before lite-07 one start answers the one range, and carries no largest.
	const position = { group: 3n, frame: 2 };
	for (const start of [
		new SubscribeStart({ id: 1n, start: 4n, largest: "unknown" }),
		new SubscribeStart({ id: 0n, largest: "unknown" }),
		new SubscribeStart({ id: 0n, start: 4n, largest: { position, ended: true } }),
	])
		await expect(encode(Version.DRAFT_06, { start })).rejects.toThrow();
	await expect(
		decodeSubscribeResponse(
			new Reader(undefined, new Uint8Array([4, 2, 0, 0]), Version.DRAFT_06),
			Version.DRAFT_06,
		),
	).rejects.toThrow(ProtocolViolation);
});

test("SubscribeStart answers one range with its start and the largest on draft-07", async () => {
	const position = { group: 3n, frame: 2 };
	// Type 4, length, Range ID, start plus one, Largest Type 2, group, frame, ended.
	const answered = new SubscribeStart({ id: 1n, start: 4n, largest: { position, ended: true } });
	expect(await encode(Version.DRAFT_07, { start: answered })).toEqual(new Uint8Array([4, 6, 1, 5, 2, 3, 2, 1]));
	// Unknown and Empty carry no fields; a start of 0 is a range with no group there.
	expect(await encode(Version.DRAFT_07, { start: new SubscribeStart({ id: 4n, largest: "unknown" }) })).toEqual(
		new Uint8Array([4, 3, 4, 0, 0]),
	);
	expect(
		await encode(Version.DRAFT_07, { start: new SubscribeStart({ id: 0n, start: 0n, largest: "empty" }) }),
	).toEqual(new Uint8Array([4, 3, 0, 1, 1]));
	for (const largest of ["unknown", "empty", { position, ended: false }, { position, ended: true }] as const) {
		for (const start of [undefined, 0n, 7n]) {
			const got = await responseRoundtrip(Version.DRAFT_07, {
				start: new SubscribeStart({ id: 2n, start, largest }),
			});
			if (!("start" in got)) throw new Error("expected start");
			expect([got.start.id, got.start.start, got.start.largest]).toEqual([2n, start, largest]);
		}
	}
	// Any other Largest Type or ended is malformed, and lite-07 leaves type 0 unused.
	for (const bytes of [
		[4, 6, 1, 5, 2, 3, 2, 2],
		[4, 3, 1, 5, 3],
		[0, 1, 4],
	]) {
		const malformed = new Reader(undefined, new Uint8Array(bytes), Version.DRAFT_07);
		await expect(decodeSubscribeResponse(malformed, Version.DRAFT_07)).rejects.toThrow(ProtocolViolation);
	}
});

test("SubscribeEnd round-trips on draft-05", async () => {
	// Type, length, exclusive final group.
	expect(await encode(Version.DRAFT_05, { end: new SubscribeEnd(7n) })).toEqual(new Uint8Array([1, 1, 7]));
	const got = await responseRoundtrip(Version.DRAFT_05, { end: new SubscribeEnd(7n) });
	expect("end" in got).toBe(true);
	if (!("end" in got)) throw new Error("expected end");
	expect(got.end.group).toBe(7n);
});

test("SubscribeEnd carries its error on draft-07", async () => {
	// Type, length, exclusive final group, then the error code + 1, 0 for a clean end.
	expect(await encode(Version.DRAFT_07, { end: new SubscribeEnd(7n) })).toEqual(new Uint8Array([1, 2, 7, 0]));
	expect(await encode(Version.DRAFT_07, { end: new SubscribeEnd(7n, 71) })).toEqual(new Uint8Array([1, 2, 7, 72]));
	const got = await responseRoundtrip(Version.DRAFT_07, { end: new SubscribeEnd(7n, 71) });
	if (!("end" in got)) throw new Error("expected end");
	expect([got.end.group, got.end.error]).toEqual([7n, 71]);
	// An older version has nowhere to carry it.
	await expect(encode(Version.DRAFT_06, { end: new SubscribeEnd(7n, 71) })).rejects.toThrow();
});

test("SubscribeDrop is restored on draft-07", async () => {
	const drop: SubscribeResponse = { drop: new SubscribeDrop({ start: 1n, end: 4n, error: 7 }) };
	const wire = await encode(Version.DRAFT_07, drop);
	// Groups 1 to 3 as a start and an exclusive end, then the code.
	expect(wire).toEqual(new Uint8Array([2, 3, 1, 4, 7]));
	const got = await responseRoundtrip(Version.DRAFT_07, drop);
	if (!("drop" in got)) throw new Error("expected drop");
	expect([got.drop.start, got.drop.end, got.drop.error]).toEqual([1n, 4n, 7]);
	// Draft-06 carries an inclusive end.
	expect(await encode(Version.DRAFT_06, drop)).toEqual(new Uint8Array([2, 3, 1, 3, 7]));
});

// A lite-07 OLD drop carries the max age it was judged under, in milliseconds plus one, so zero names
// the publisher's own. No other drop, and no earlier version, carries one. Mirrors the Rust
// `a_lite07_old_drop_carries_its_judged_max_age`.
test("a lite-07 OLD drop carries its judged max age", async () => {
	for (const [maxAge, wire] of [
		[undefined, 0],
		[0, 1],
		[9, 10],
	] as const) {
		const drop: SubscribeResponse = { drop: SubscribeDrop.stale(1n, 4n, maxAge, Version.DRAFT_07) };
		expect(await encode(Version.DRAFT_07, drop)).toEqual(new Uint8Array([2, 4, 1, 4, StreamCode.Old, wire]));
		const got = await responseRoundtrip(Version.DRAFT_07, drop);
		if (!("drop" in got)) throw new Error("expected drop");
		expect([got.drop.error, got.drop.maxAge]).toEqual([StreamCode.Old, maxAge]);
	}
	// A drop depending on no budget settles every reader.
	expect(SubscribeDrop.given(1n, 4n, new StreamError(StreamCode.Old), Version.DRAFT_07).maxAge).toBeUndefined();
	const skip: SubscribeResponse = { drop: SubscribeDrop.stale(1n, 4n, 9, Version.DRAFT_07) };
	skip.drop.error = StreamCode.Skip;
	expect(await encode(Version.DRAFT_07, skip)).toEqual(new Uint8Array([2, 3, 1, 4, 0x3a]));
	const lite06: SubscribeResponse = { drop: SubscribeDrop.stale(1n, 4n, 9, Version.DRAFT_06) };
	expect(await encode(Version.DRAFT_06, lite06)).toEqual(new Uint8Array([2, 3, 1, 3, 0]));
});

test("lite-07 carries skips in SubscribeDrop(SKIP) and rejects type 3", async () => {
	const skip: SubscribeResponse = { drop: new SubscribeDrop({ start: 1n, end: 4n, error: StreamCode.Skip }) };
	expect(await encode(Version.DRAFT_07, skip)).toEqual(new Uint8Array([2, 3, 1, 4, 0x3a]));
	const got = await responseRoundtrip(Version.DRAFT_07, skip);
	if (!("drop" in got)) throw new Error("expected drop");
	expect([got.drop.start, got.drop.end, got.drop.error]).toEqual([1n, 4n, StreamCode.Skip]);
	for (const version of [Version.DRAFT_06, Version.DRAFT_07]) {
		await expect(
			decodeSubscribeResponse(new Reader(undefined, new Uint8Array([3, 2, 1, 4]), version), version),
		).rejects.toThrow(ProtocolViolation);
	}
});

// Lite-07 carries a non-empty `[Group Start, Group End)`, which reaches but never passes the
// reserved group 2^64-1.
test("lite-07 drop spans are non-empty and stop at the reserved group", async () => {
	const top = (1n << 64n) - 1n;
	for (const [start, end] of [
		[0n, 1n],
		[3n, 4n],
		[0n, top],
		[top - 1n, top],
	]) {
		for (const error of [StreamCode.Skip, 7]) {
			const drop = await responseRoundtrip(Version.DRAFT_07, { drop: new SubscribeDrop({ start, end, error }) });
			if (!("drop" in drop)) throw new Error("expected drop");
			expect([drop.drop.start, drop.drop.end, drop.drop.error]).toEqual([start, end, error]);
		}
	}

	const decode = (body: number[]) =>
		decodeSubscribeResponse(new Reader(undefined, new Uint8Array(body), Version.DRAFT_07), Version.DRAFT_07);
	// A Group End not past its Group Start is a protocol violation, empty or reversed.
	await expect(decode([2, 3, 4, 4, 7])).rejects.toThrow(ProtocolViolation);
	await expect(decode([2, 3, 4, 3, 7])).rejects.toThrow(ProtocolViolation);

	// An empty or reversed span has no encoding.
	for (const [start, end] of [
		[3n, 3n],
		[3n, 1n],
	]) {
		await expect(encode(Version.DRAFT_07, { drop: new SubscribeDrop({ start, end, error: 0 }) })).rejects.toThrow(
			RangeError,
		);
	}
	await expect(
		encode(Version.DRAFT_06, { drop: new SubscribeDrop({ start: 3n, end: 1n, error: 0 }) }),
	).rejects.toThrow();
});

test("SubscribeDrop is type 0x2 on draft-05 and 0x1 on draft-04", async () => {
	const drop: SubscribeResponse = { drop: new SubscribeDrop({ start: 1n, end: 4n, error: 0 }) };

	const wire05 = await encode(Version.DRAFT_05, drop);
	expect(wire05[0]).toBe(2);

	const wire04 = await encode(Version.DRAFT_04, drop);
	expect(wire04[0]).toBe(1);

	const got = await responseRoundtrip(Version.DRAFT_05, drop);
	expect("drop" in got).toBe(true);
	if (!("drop" in got)) throw new Error("expected drop");
	expect([got.drop.start, got.drop.end]).toEqual([1n, 4n]);
});

test("SUBSCRIBE_OK is rejected on draft-05", async () => {
	await expect(encode(Version.DRAFT_05, { ok: new SubscribeOk({ priority: 1 }) })).rejects.toThrow();
});

test("frame bounds without their group bounds are rejected before encoding", async () => {
	const base = {
		id: 1n,
		broadcast: Path.from("room"),
		track: "video",
		priority: 0,
	};
	await expect(encodeSubscribe(new Subscribe({ ...base, startFrame: 3 }))).rejects.toThrow(
		"frame bound without a group bound",
	);
	await expect(encodeSubscribe(new Subscribe({ ...base, endFrame: 7 }))).rejects.toThrow(
		"frame bound without a group bound",
	);
});

test("model group ends convert to the wire without an off-by-one", () => {
	expect(inclusiveGroupEnd(undefined)).toBeUndefined();
	expect(inclusiveGroupEnd(1n)).toBe(0n);
	expect(inclusiveGroupEnd(10n)).toBe(9n);
	expect(() => inclusiveGroupEnd(0n)).toThrow("empty subscription range cannot be encoded");
});

// Lite-07 carries each range's `Priority` then `Group Order`, on a SUBSCRIBE's ranges and a
// SUBSCRIBE_UPDATE's retunes alike, and refuses an order it does not define. Mirrors the Rust
// `range_policy_roundtrips_on_lite07`.
test.each(["ascending", "descending"] as const)("lite-07 round-trips a range at a %s group order", async (order) => {
	const version = Version.DRAFT_07;
	const range = withPolicy({ start: { time: 5n } }, { priority: 3, order });
	const subscribe = new Subscribe({
		id: 1n,
		broadcast: Path.from("room"),
		track: "video",
		priority: 0,
		ranges: [range],
	});
	const got = await Subscribe.decode(
		new Reader(undefined, await encodeMessage(version, subscribe), version),
		version,
	);
	expect(got.ranges).toEqual([range]);
	// The subscription as a whole runs at its one range's policy.
	expect([got.priority, got.order]).toEqual([3, order]);

	const retune: [bigint, Retune][] = [[0n, { priority: 3, order, max: null }]];
	const update = await encodeMessage(version, new SubscribeUpdate({ priority: 0, ranges: [], retune }));
	// The length prefix, no range to retire or add, one retune: its Range ID, priority, order, then
	// no max age, then no wanted group.
	expect([...update]).toEqual([8, 0, 0, 1, 0, 3 + 128, order === "ascending" ? 1 : 0, 0, 0]);
	const decoded = await SubscribeUpdate.decode(new Reader(undefined, update, version), version);
	expect(decoded.retune).toEqual(retune);

	const invalid = new Uint8Array(update);
	invalid[6] = 2;
	await expect(SubscribeUpdate.decode(new Reader(undefined, invalid, version), version)).rejects.toThrow(
		ProtocolViolation,
	);
});

// Ranges at different policies: the subscription runs at the highest priority, newest first if any
// range there asks so.
test("a subscription runs at its most urgent range's policy", () => {
	expect(
		subscriptionPolicy([
			{ priority: 1, order: "ascending" },
			{ priority: 5, order: "ascending" },
		]),
	).toEqual({ priority: 5, order: "ascending" });
	expect(subscriptionPolicy([{ priority: 5, order: "ascending" }, { priority: 5 }])).toEqual({
		priority: 5,
		order: "descending",
	});
	expect(subscriptionPolicy([])).toEqual({ priority: 0, order: "descending" });
});

// A group goes out at the highest priority among the ranges still wanting it, at that priority's
// order with newest first breaking a tie, and falls to the next once it ages past the budget of
// every range at the top.
test("lite draft-07: a group goes out at the top range still wanting it", () => {
	const policies = new Map<bigint, Policy>([
		[0n, { priority: 5, order: "ascending" }],
		[1n, { priority: 5, order: "descending" }],
		[2n, { priority: 1, order: "ascending" }],
	]);
	const steer = { whole: { priority: 9, order: "ascending" as const }, policy: (id: bigint) => policies.get(id) };
	const [short, long] = [1000, 2000];

	const covers: [bigint, number][] = [
		[0n, long],
		[1n, short],
		[2n, NO_MAX_DELAY],
	];
	// Newest first breaks a tie.
	expect(groupPolicy(steer, covers)).toEqual({ policy: { priority: 5, order: "descending" }, lasts: short });
	// The order of the range left at the top.
	expect(groupPolicy(steer, covers, short)).toEqual({ policy: { priority: 5, order: "ascending" }, lasts: long });
	// History keeps the group at its own priority.
	expect(groupPolicy(steer, covers, long)).toEqual({ policy: { priority: 1, order: "ascending" } });
	// A range ranked below that ages first lowers nothing.
	expect(
		groupPolicy(steer, [
			[0n, long],
			[2n, short],
		]),
	).toEqual({ policy: { priority: 5, order: "ascending" } });
	// A retired range leaves the subscription's.
	expect(groupPolicy(steer, [[7n, long]])).toEqual({ policy: steer.whole });
	expect(groupPolicy(steer, [])).toEqual({ policy: steer.whole });
});

// No earlier wire has `Group Order`, so an ascending subscription falls back to descending,
// byte-identical to one that never asked.
test.each([Version.DRAFT_03, Version.DRAFT_05, Version.DRAFT_06])(
	"version %d falls back to a descending group order",
	async (version) => {
		const props = { id: 1n, broadcast: Path.from("room"), track: "video", priority: 3, maxDelay: 250 };
		const descending = await encodeMessage(version, new Subscribe(props));
		const ascending = await encodeMessage(version, new Subscribe({ ...props, order: "ascending" }));
		expect(ascending).toEqual(descending);
		const got = await Subscribe.decode(new Reader(undefined, ascending, version), version);
		expect(got.order).toBe("descending");

		const update = await encodeMessage(version, new SubscribeUpdate({ priority: 3, order: "ascending" }));
		expect(update).toEqual(await encodeMessage(version, new SubscribeUpdate({ priority: 3 })));
		const decoded = await SubscribeUpdate.decode(new Reader(undefined, update, version), version);
		expect(decoded.order).toBe("descending");
	},
);

// Lite-07 carries a live join's lag as a start Anchor in whole milliseconds, on SUBSCRIBE and
// SUBSCRIBE_UPDATE alike, and an end refuses it. An older wire carries it as no start. Mirrors the
// Rust `a_live_start_roundtrips_only_on_lite07`.
test("a live start round-trips only on lite-07", async () => {
	const version = Version.DRAFT_07;
	for (const min of [0, 250, Number.MAX_SAFE_INTEGER]) {
		const ranges: WireRange[] = [{ start: { live: { min, max: null } }, end: { time: 9n } }];
		const props = { id: 1n, broadcast: Path.from("room"), track: "video", priority: 3, ranges };
		const subscribe = await encodeMessage(version, new Subscribe(props));
		expect((await Subscribe.decode(new Reader(undefined, subscribe, version), version)).ranges).toEqual(ranges);
		const update = await encodeMessage(version, new SubscribeUpdate({ priority: 3, ranges }));
		expect((await SubscribeUpdate.decode(new Reader(undefined, update, version), version)).ranges).toEqual(ranges);
	}
	// The model rounds a lag up to whole milliseconds, as the wire carries it.
	expect(toWireRanges(rangesOf([{ start: { live: { min: Duration.micros(1500), max: Duration.ZERO } } }]))).toEqual([
		{ start: { live: { min: 2, max: 0 } } },
	]);
	expect(requestedRanges({ startFrame: 0, ranges: [{ start: { live: { min: 250, max: null } } }] })).toEqual([
		{ start: { live: { min: Duration.millis(250), max: null } } },
	]);

	// No start on a wire without the Anchor, which carries only the span.
	const legacy = Version.DRAFT_06;
	const span = wireSpan([{ start: { live: { min: Duration.millis(250), max: Duration.ZERO } } }]);
	expect(span).toEqual({ start: undefined, end: undefined });
	const props = { id: 1n, broadcast: Path.from("room"), track: "video", priority: 3, startGroup: span.start };
	const old = await Subscribe.decode(
		new Reader(undefined, await encodeMessage(legacy, new Subscribe(props)), legacy),
		legacy,
	);
	expect(old.startGroup).toBeUndefined();

	// A live end is refused both ways.
	await expect(
		encodeMessage(
			version,
			new SubscribeUpdate({ priority: 3, ranges: [{ end: { live: { min: 250, max: null } } as never }] }),
		),
	).rejects.toThrow();
	// A range ending at time 250 has the shape of one starting live 250 ms back, but for the Anchor
	// Types: turn its end's Time (1) into Live (3).
	const timed = await encodeMessage(version, new SubscribeUpdate({ priority: 3, ranges: [{ end: { time: 250n } }] }));
	const live = await encodeMessage(
		version,
		new SubscribeUpdate({ priority: 3, ranges: [{ start: { live: { min: 250, max: null } } }] }),
	);
	const at = timed.findIndex((byte, index) => byte === 0 && live[index] === 3);
	expect(timed[at + 1]).toBe(1);
	const ended = Uint8Array.from(timed);
	ended[at + 1] = 3;
	await expect(SubscribeUpdate.decode(new Reader(undefined, ended, version), version)).rejects.toThrow(
		ProtocolViolation,
	);
});

// Subscriber Max Age is bare milliseconds before lite-07, so no limit goes out as the largest
// value every peer reads and reads back as no limit. Lite-01/02 carry none, and lite-07 none
// either, since each live start carries its own. Mirrors the Rust
// `max_delay_is_carried_only_before_lite07`.
test("max delay is carried only before lite-07", async () => {
	// After the size, the priority, and the order: lite-07 goes straight on to its ranges.
	const field = async (version: Version, maxDelay: number | null) =>
		(await encodeMessage(version, new SubscribeUpdate({ priority: 0, maxDelay, ranges: [] }))).slice(3);
	expect(await field(Version.DRAFT_07, 250)).toEqual(await field(Version.DRAFT_07, null));

	for (const version of [Version.DRAFT_01, Version.DRAFT_03, Version.DRAFT_06, Version.DRAFT_07]) {
		for (const maxDelay of [null, 0, 250, Number.MAX_SAFE_INTEGER - 1]) {
			const expected = {
				[Version.DRAFT_01]: null,
				[Version.DRAFT_03]: maxDelay,
				[Version.DRAFT_06]: maxDelay,
				[Version.DRAFT_07]: null,
			}[version];
			const props = { id: 1n, broadcast: Path.from("room"), track: "video", priority: 3, maxDelay };
			const subscribe = await encodeMessage(version, new Subscribe(props));
			expect((await Subscribe.decode(new Reader(undefined, subscribe, version), version)).maxDelay).toBe(
				expected,
			);
			const update = await encodeMessage(version, new SubscribeUpdate({ priority: 3, maxDelay, ranges: [] }));
			expect((await SubscribeUpdate.decode(new Reader(undefined, update, version), version)).maxDelay).toBe(
				expected,
			);
		}
	}
});

// Rust reads any bare-milliseconds max delay at or past the sentinel as no limit, so one too wide
// for a JS number does too rather than failing the decode.
test("a max delay past the largest safe value before lite-07 is no limit", async () => {
	for (const version of [Version.DRAFT_03, Version.DRAFT_06]) {
		// A max delay of 0 is a one-byte varint, found where an update with 1 differs.
		const encode = (maxDelay: number) =>
			encodeMessage(version, new SubscribeUpdate({ priority: 0, maxDelay, ranges: [] }));
		const update = await encode(0);
		const other = await encode(1);
		const at = update.findIndex((byte, index) => byte !== other[index]);
		for (const millis of [2n ** 53n, 2n ** 62n - 1n]) {
			const wide = new Uint8Array(8);
			new DataView(wide.buffer).setBigUint64(0, millis | (0b11n << 62n));
			const bytes = Uint8Array.from([
				update[0] + 7,
				...update.subarray(1, at),
				...wide,
				...update.subarray(at + 1),
			]);
			expect((await SubscribeUpdate.decode(new Reader(undefined, bytes, version), version)).maxDelay).toBeNull();
		}
	}
});

test("optional subscriber milliseconds reject unsafe budgets", async () => {
	for (const maxDelay of [-1, 0.5, Number.MAX_SAFE_INTEGER + 1]) {
		await expect(
			encodeMessage(
				Version.DRAFT_06,
				new Subscribe({ id: 1n, broadcast: Path.from("room"), track: "video", priority: 3, maxDelay }),
			),
		).rejects.toThrow(RangeError);
		await expect(encodeMessage(Version.DRAFT_06, new SubscribeUpdate({ priority: 3, maxDelay }))).rejects.toThrow(
			RangeError,
		);
	}
});

test("wanted group survives lite-07 and disappears on legacy wires", async () => {
	for (const version of [Version.DRAFT_05, Version.DRAFT_06, Version.DRAFT_07]) {
		for (const wantedGroup of [undefined, 0n, 3n, BigInt(Number.MAX_SAFE_INTEGER)]) {
			const update = new SubscribeUpdate({ priority: 7, wantedGroup, ranges: [] });
			const encoded = await encodeMessage(version, update);
			const decoded = await SubscribeUpdate.decode(new Reader(undefined, encoded, version), version);
			expect(decoded.wantedGroup).toBe(version === Version.DRAFT_07 ? wantedGroup : undefined);
		}
	}
});

test("wanted groups refuse malformed input before writing bytes", async () => {
	for (const wantedGroup of [-1n]) {
		expect(() => new SubscribeUpdate({ priority: 0, wantedGroup })).toThrow(RangeError);
		const update = new SubscribeUpdate({ priority: 0 });
		update.wantedGroup = wantedGroup;
		await expect(encodeMessage(Version.DRAFT_07, update)).rejects.toThrow(RangeError);
	}
});

// Before lite-07 a range is an inclusive Group Start and Group End, refused reversed. Mirrors the
// Rust `drop_ranges_before_lite07_are_inclusive`.
for (const version of [Version.DRAFT_05, Version.DRAFT_06]) {
	test(`SUBSCRIBE_DROP ranges are inclusive on ${version}`, async () => {
		// Group Start 3, Group End 1.
		const malformed = new Reader(undefined, new Uint8Array([3, 3, 1, 0]), version);
		await expect(SubscribeDrop.decode(malformed, version)).rejects.toThrow("invalid group range");
		const max = (1n << 62n) - 1n;
		for (const [start, end] of [
			[2n, 2n],
			[1n, 4n],
			[max, max + 1n],
		]) {
			const got = await responseRoundtrip(version, { drop: new SubscribeDrop({ start, end, error: 0 }) });
			if (!("drop" in got)) throw new Error("missing drop");
			expect([got.drop.start, got.drop.end]).toEqual([start, end]);
		}
	});
}

// An update sends only the ranges that changed, and the publisher resolves the same list, with the
// same Range IDs, as the subscriber holds. Mirrors the Rust `lite07_updates_edit_only_the_changed_ranges`.
test("lite-07 updates edit only the changed ranges", async () => {
	const version = Version.DRAFT_07;
	const a = { start: { time: 1n }, end: { time: 4n } };
	const b = {};
	const c = { start: { time: 9_000n } };
	const sent = new LiveRanges(version, [a, b]);
	const live = new LiveRanges(version, [a, b]);
	const step = async (ranges: WireRange[], remove: bigint[], add: WireRange[], renew?: Set<bigint>) => {
		const edit = sent.edit(ranges, {}, renew);
		expect(edit).toEqual({ remove, add, retune: [], fillIds: [], probeIds: [] });
		const update = new SubscribeUpdate({ priority: 0, remove: edit.remove, ranges: edit.add });
		const got = await SubscribeUpdate.decode(
			new Reader(undefined, await encodeMessage(version, update), version),
			version,
		);
		live.apply(got);
		expect(live.entries()).toEqual(sent.entries());
	};
	await step([b, a], [], []);
	await step([a, c], [1n], [c]);
	await step([c, c, a], [], [c]);
	await step([b], [0n, 2n, 3n], [b]);
	expect(live.entries()).toEqual([[4n, b]]);
	// One renewed is retired and added again though still wanted, so the publisher resolves it afresh.
	await step([b], [4n], [b], new Set([4n]));
	expect(live.entries()).toEqual([[5n, b]]);

	// An id never assigned or already retired, or nothing left live, is refused.
	const refused = (remove: bigint[], ranges: WireRange[]) =>
		new LiveRanges(version, [a]).apply(new SubscribeUpdate({ priority: 0, remove, ranges }));
	expect(() => refused([1n], [a])).toThrow(ProtocolViolation);
	expect(() => refused([0n, 0n], [a])).toThrow(ProtocolViolation);
	expect(() => refused([0n], [])).toThrow(ProtocolViolation);
});

// An update retunes a live join wanted under another max alone in place, keeping its Range ID and
// so where it was placed; anything else about it changing replaces it. The publisher refuses a
// retune naming no live join, or one twice. Mirrors the Rust `lite07_retunes_a_live_join_in_place`.
test("lite-07 retunes a live join in place", async () => {
	const version = Version.DRAFT_07;
	const live = (min: number, max: number | null): WireRange => ({ start: { live: { min, max } } });
	const time: WireRange = { start: { time: 5n } };
	const sent = new LiveRanges(version, [live(100, 500), time]);
	const side = new LiveRanges(version, [live(100, 500), time]);

	const edit = sent.edit([live(100, 2000), time]);
	expect(edit).toEqual({ remove: [], add: [], retune: [[0n, max(2000)]], fillIds: [], probeIds: [] });
	const update = new SubscribeUpdate({ priority: 0, remove: edit.remove, ranges: edit.add, retune: edit.retune });
	const got = await SubscribeUpdate.decode(
		new Reader(undefined, await encodeMessage(version, update), version),
		version,
	);
	expect(side.apply(got)).toEqual({ retired: [], added: [], retuned: [[0n, live(100, 2000)]] });
	expect(side.entries()).toEqual(sent.entries());

	// Another lag is another join, placed afresh.
	const lag = sent.edit([live(200, 2000), time]);
	expect(lag).toEqual({ remove: [0n], add: [live(200, 2000)], retune: [], fillIds: [], probeIds: [] });
	side.apply(new SubscribeUpdate({ priority: 0, remove: lag.remove, ranges: lag.add }));

	const retune = (ranges: LiveRanges, retune: [bigint, Retune][]) =>
		ranges.apply(new SubscribeUpdate({ priority: 0, remove: [], ranges: [], retune }));
	// Fresh copies of what `side` holds, since a refused edit ends the session: 0 retired, 1 a time
	// start, 2 a live join.
	const held = () => {
		const ranges = new LiveRanges(version, [live(100, 500), time, live(200, 2000)]);
		ranges.apply(new SubscribeUpdate({ priority: 0, remove: [0n], ranges: [] }));
		return ranges;
	};
	expect(held().entries()).toEqual(side.entries());
	// A time start has no max age, but takes a new policy.
	expect(() => retune(held(), [[1n, max(5)]])).toThrow(ProtocolViolation);
	expect(retune(held(), [[1n, { priority: 2, order: "ascending", max: null }]]).retuned).toEqual([
		[1n, { ...time, priority: 2, order: "ascending" }],
	]);
	expect(() => retune(held(), [[0n, max(null)]])).toThrow(ProtocolViolation);
	expect(() =>
		retune(held(), [
			[2n, max(null)],
			[2n, max(null)],
		]),
	).toThrow(ProtocolViolation);
	expect(retune(side, [[2n, max(null)]]).retuned).toEqual([[2n, live(200, null)]]);
});

// A placed join retunes its max age in place like a live one, kept where it was placed; one placed
// elsewhere is another range. Mirrors the Rust `lite07_retunes_a_placed_join_in_place`.
test("lite-07 retunes a placed join in place", () => {
	const version = Version.DRAFT_07;
	const joined = (absolute: bigint, max: number | null): WireRange => ({
		start: { joined: { absolute, frame: 2, max } },
	});
	const sent = new LiveRanges(version, [joined(5n, 500)]);
	const side = new LiveRanges(version, [joined(5n, 500)]);
	const edit = sent.edit([joined(5n, null)]);
	expect(edit).toEqual({ remove: [], add: [], retune: [[0n, max(null)]], fillIds: [], probeIds: [] });
	side.apply(new SubscribeUpdate({ priority: 0, remove: edit.remove, ranges: edit.add, retune: edit.retune }));
	expect(side.entries()).toEqual([[0n, joined(5n, null)]]);

	expect(sent.edit([joined(6n, null)])).toEqual({
		remove: [0n],
		add: [joined(6n, null)],
		retune: [],
		fillIds: [],
		probeIds: [],
	});
});

// A live start's max rides beside its lag, in milliseconds plus one, rounded up, 0 for no limit.
// Mirrors the Rust `a_live_starts_max_age_roundtrips_on_lite07`.
test("a live start's max round-trips on lite-07", async () => {
	const version = Version.DRAFT_07;
	for (const [max, wire] of [
		[null, null],
		[Duration.ZERO, 0],
		[Duration.millis(250), 250],
		[Duration.micros(1500), 2],
	] as const) {
		const ranges = toWireRanges(rangesOf([{ start: { live: { min: Duration.millis(100), max } } }]));
		expect(ranges).toEqual([{ start: { live: { min: 100, max: wire } } }]);
		const props = { id: 1n, broadcast: Path.from("room"), track: "video", priority: 3, ranges };
		const subscribe = await encodeMessage(version, new Subscribe(props));
		const got = await Subscribe.decode(new Reader(undefined, subscribe, version), version);
		expect(got.ranges).toEqual(ranges);
		expect(requestedRanges(got)).toEqual([
			{ start: { live: { min: Duration.millis(100), max: wire === null ? null : Duration.millis(wire) } } },
		]);
	}
});

// An edit resolves to the Range IDs it names. Mirrors the Rust `lite07_an_edit_names_its_range_ids`.
test("lite-07 an edit names its range ids", () => {
	const live = new LiveRanges(Version.DRAFT_07, [{}]);
	const range = (group: bigint): WireRange => ({ start: { absolute: group } });
	const edit = (remove: bigint[], ranges: WireRange[]) =>
		live.apply(new SubscribeUpdate({ priority: 0, remove, ranges }));
	expect(edit([], [])).toEqual({ retired: [], added: [], retuned: [] });
	expect(edit([0n], [range(4n), range(5n)])).toEqual({
		retired: [0n],
		added: [
			[1n, range(4n)],
			[2n, range(5n)],
		],
		retuned: [],
	});
	expect(edit([1n], [range(6n)])).toEqual({ retired: [1n], added: [[3n, range(6n)]], retuned: [] });
	expect(live.entries()).toEqual([
		[2n, range(5n)],
		[3n, range(6n)],
	]);
});

// An edit to ranges too large for a SUBSCRIBE is refused before anything changes, so the live
// ranges stay those last sent and the next edit is made against them. Mirrors the Rust
// `lite07_a_refused_edit_leaves_the_ranges_as_sent`.
test("lite-07 a refused edit leaves the ranges as sent", () => {
	const live = new LiveRanges(Version.DRAFT_07, [{}]);
	const wide: WireRange[] = Array.from({ length: Math.floor(MAX_MESSAGE_SIZE / 4) }, (_, group) => ({
		start: { absolute: BigInt(group) << 20n },
		end: { absolute: (BigInt(group) << 20n) + 1n },
	}));
	expect(() => live.edit(wide)).toThrow("too many ranges");
	expect(live.entries()).toEqual([[0n, {}]]);
	expect(live.next()).toBe(1n);
	expect(live.edit(wide.slice(0, 2))).toEqual({
		remove: [0n],
		add: wide.slice(0, 2),
		retune: [],
		fillIds: [],
		probeIds: [],
	});
});

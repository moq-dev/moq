import { expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import { ProtocolViolation } from "../error.ts";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import { rangesOf, wireSpan } from "../window.ts";
import { MAX_MESSAGE_SIZE } from "./message.ts";
import {
	decodeSubscribeResponse,
	encodeSubscribeResponse,
	inclusiveGroupEnd,
	LiveRanges,
	positionFrames,
	rangeSize,
	requestedRanges,
	Subscribe,
	SubscribeDrop,
	SubscribeFinish,
	SubscribeOk,
	type SubscribeResponse,
	SubscribeSkip,
	SubscribeStart,
	SubscribeStarted,
	SubscribeUpdate,
	spanOf,
	toWireRanges,
	type WireRange,
} from "./subscribe.ts";
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
	// Priority, order, and max delay as SubscribeUpdate writes them, then no Range ID to retire,
	// the ranges it adds, and an absent wanted group.
	const head = (await encodeMessage(Version.DRAFT_07, new SubscribeUpdate({ priority: 0, ranges: [] }))).slice(1, 4);
	const update = async (ranges: number[]) => {
		const body = [...head, 0, ...ranges, 0];
		const got = await SubscribeUpdate.decode(
			new Reader(undefined, new Uint8Array([body.length, ...body]), Version.DRAFT_07),
			Version.DRAFT_07,
		);
		return { ...got, ...spanOf(Version.DRAFT_07, got.ranges ?? []) };
	};

	// Type 1 is a time, both ends inclusive, and type 0 none. A time names the group it keys, so
	// the span covers it, and the model reads it as the same time, the end inclusive.
	const timed = await update([2, 1, 40, 1, 90, 0, 0]);
	expect(timed.ranges).toEqual([{ start: { time: 40n }, end: { time: 90n } }, {}]);
	expect([timed.startGroup, timed.endGroup]).toEqual([undefined, undefined]);
	expect(requestedRanges(timed)).toEqual([{ start: 40n, end: 90n }, {}]);
	const bounded = await update([1, 1, 40, 1, 90]);
	expect([bounded.startGroup, bounded.endGroup]).toEqual([40n, 90n]);

	// One ending before its start covers no group there, and keeps its place.
	const reversed = await update([1, 1, 5, 1, 3]);
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
	// Type 3 is a live join's lag, which only a start may carry.
	expect((await update([1, 3, 7, 0])).ranges).toEqual([{ start: { live: 7 } }]);
	await expect(update([1, 0, 3, 7])).rejects.toThrow(ProtocolViolation);
	// Any other type is a violation, and so is a Position at frame 0 or as an end.
	await expect(update([1, 2, 0, 0, 0])).rejects.toThrow(ProtocolViolation);
	await expect(update([1, 1, 0, 2, 7, 4])).rejects.toThrow(ProtocolViolation);
	await expect(update([1, 4, 0, 0])).rejects.toThrow(ProtocolViolation);
});

test("a lite-07 range may start partway through a group, which the span keeps", async () => {
	const position = { start: { absolute: 7n, frame: 4 } };
	const update = new SubscribeUpdate({ priority: 0, ranges: [position] });
	const bytes = await encodeMessage(Version.DRAFT_07, update);
	// One range: type 2, start 7, frame 4, then no end, and an absent wanted group.
	expect(Array.from(bytes.slice(-6))).toEqual([1, 2, 7, 4, 0, 0]);
	expect(rangeSize(position)).toBe(4);
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

	// An end there can't go out.
	await expect(
		encodeMessage(
			Version.DRAFT_07,
			new SubscribeUpdate({ priority: 0, ranges: [{ end: { absolute: 9n, frame: 2 } }] }),
		),
	).rejects.toThrow("a range ends only at a group");
});

test("lite-07 sends an absolute anchor as a time, an end as the time before it", async () => {
	const update = new SubscribeUpdate({
		priority: 0,
		ranges: [{ start: { absolute: 7n }, end: { absolute: 10n } }],
	});
	const got = await SubscribeUpdate.decode(
		new Reader(undefined, await encodeMessage(Version.DRAFT_07, update), Version.DRAFT_07),
		Version.DRAFT_07,
	);
	expect(got.ranges).toEqual([{ start: { time: 7n }, end: { time: 9n } }]);
	// Nothing comes before group 0, so an end there can't go out.
	await expect(
		encodeMessage(Version.DRAFT_07, new SubscribeUpdate({ priority: 0, ranges: [{ end: { absolute: 0n } }] })),
	).rejects.toThrow("empty subscription range cannot be encoded");
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
	const got = await responseRoundtrip(Version.DRAFT_05, { start: new SubscribeStart([42n]) });
	expect("start" in got).toBe(true);
	if (!("start" in got)) throw new Error("expected start");
	expect(got.start.starts).toEqual([42n]);
});

test("SubscribeStart carries the largest position and whether it ended on draft-07", async () => {
	const position = { group: 3n, frame: 2 };
	// Type, length, largest group + 1, largest frame, ended, one range start plus one.
	expect(await encode(Version.DRAFT_07, { start: new SubscribeStart([4n], { position, ended: true }) })).toEqual(
		new Uint8Array([0, 5, 4, 2, 1, 1, 5]),
	);
	for (const largest of [undefined, { position, ended: false }, { position, ended: true }]) {
		const got = await responseRoundtrip(Version.DRAFT_07, { start: new SubscribeStart([4n], largest) });
		if (!("start" in got)) throw new Error("expected start");
		expect(got.start.largest).toEqual(largest);
	}
	// Any other ended is malformed.
	const malformed = new Reader(undefined, new Uint8Array([0, 5, 4, 2, 2, 1, 5]), Version.DRAFT_07);
	await expect(decodeSubscribeResponse(malformed, Version.DRAFT_07)).rejects.toThrow(ProtocolViolation);
	// Draft-06 has no largest position on the wire.
	expect(await encode(Version.DRAFT_06, { start: new SubscribeStart([4n], { position, ended: true }) })).toEqual(
		new Uint8Array([0, 1, 4]),
	);
});

test("SubscribeStart carries each range's start in place of one group on draft-07", async () => {
	const start = new SubscribeStart([2n, undefined, 0n]);
	// Type, length, no largest, three starts each plus one with 0 for none.
	expect(await encode(Version.DRAFT_07, { start })).toEqual(new Uint8Array([0, 5, 0, 3, 3, 0, 1]));
	const got = await responseRoundtrip(Version.DRAFT_07, { start });
	if (!("start" in got)) throw new Error("expected start");
	expect(got.start.starts).toEqual([2n, undefined, 0n]);
	// Draft-06's one `Group` names a single range.
	await expect(encode(Version.DRAFT_06, { start })).rejects.toThrow();
});

test("SubscribeFinish round-trips on draft-05", async () => {
	// Type, length, exclusive final group.
	expect(await encode(Version.DRAFT_05, { finish: new SubscribeFinish(7n) })).toEqual(new Uint8Array([1, 1, 7]));
	const got = await responseRoundtrip(Version.DRAFT_05, { finish: new SubscribeFinish(7n) });
	expect("finish" in got).toBe(true);
	if (!("finish" in got)) throw new Error("expected end");
	expect(got.finish.group).toBe(7n);
});

test("SubscribeFinish carries its error on draft-07", async () => {
	// Type, length, exclusive final group, then the error code + 1, 0 for a clean end.
	expect(await encode(Version.DRAFT_07, { finish: new SubscribeFinish(7n) })).toEqual(new Uint8Array([1, 2, 7, 0]));
	expect(await encode(Version.DRAFT_07, { finish: new SubscribeFinish(7n, 71) })).toEqual(
		new Uint8Array([1, 2, 7, 72]),
	);
	const got = await responseRoundtrip(Version.DRAFT_07, { finish: new SubscribeFinish(7n, 71) });
	if (!("finish" in got)) throw new Error("expected end");
	expect([got.finish.group, got.finish.error]).toEqual([7n, 71]);
	// An older version has nowhere to carry it.
	await expect(encode(Version.DRAFT_06, { finish: new SubscribeFinish(7n, 71) })).rejects.toThrow();
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

test("SubscribeSkip is type 3 on draft-07 alone", async () => {
	const skip: SubscribeResponse = { skip: new SubscribeSkip({ start: 1n, end: 4n }) };
	expect(await encode(Version.DRAFT_07, skip)).toEqual(new Uint8Array([3, 2, 1, 4]));
	const got = await responseRoundtrip(Version.DRAFT_07, skip);
	if (!("skip" in got)) throw new Error("expected skip");
	expect([got.skip.start, got.skip.end]).toEqual([1n, 4n]);
	await expect(encode(Version.DRAFT_06, skip)).rejects.toThrow();
});

// Lite-07 carries a non-empty `[Group Start, Group End)`, which reaches but never passes the
// reserved group 2^64-1.
test("lite-07 drop and skip spans are non-empty and stop at the reserved group", async () => {
	const top = (1n << 64n) - 1n;
	for (const [start, end] of [
		[0n, 1n],
		[3n, 4n],
		[0n, top],
		[top - 1n, top],
	]) {
		const skip = await responseRoundtrip(Version.DRAFT_07, { skip: new SubscribeSkip({ start, end }) });
		if (!("skip" in skip)) throw new Error("expected skip");
		expect([skip.skip.start, skip.skip.end]).toEqual([start, end]);
		const drop = await responseRoundtrip(Version.DRAFT_07, { drop: new SubscribeDrop({ start, end, error: 7 }) });
		if (!("drop" in drop)) throw new Error("expected drop");
		expect([drop.drop.start, drop.drop.end, drop.drop.error]).toEqual([start, end, 7]);
	}

	const decode = (body: number[]) =>
		decodeSubscribeResponse(new Reader(undefined, new Uint8Array(body), Version.DRAFT_07), Version.DRAFT_07);
	// A Group End not past its Group Start is a protocol violation, empty or reversed.
	await expect(decode([3, 2, 4, 4])).rejects.toThrow(ProtocolViolation);
	await expect(decode([3, 2, 4, 3])).rejects.toThrow(ProtocolViolation);
	await expect(decode([2, 3, 4, 4, 7])).rejects.toThrow(ProtocolViolation);
	await expect(decode([2, 3, 4, 3, 7])).rejects.toThrow(ProtocolViolation);

	// An empty or reversed span has no encoding.
	for (const [start, end] of [
		[3n, 3n],
		[3n, 1n],
	]) {
		await expect(encode(Version.DRAFT_07, { skip: new SubscribeSkip({ start, end }) })).rejects.toThrow(RangeError);
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

// Lite-07 carries `Group Order` right after the priority, on SUBSCRIBE and SUBSCRIBE_UPDATE
// alike, and refuses a value it does not define.
test.each(["ascending", "descending"] as const)("lite-07 round-trips a %s group order", async (order) => {
	const version = Version.DRAFT_07;
	const subscribe = new Subscribe({ id: 1n, broadcast: Path.from("room"), track: "video", priority: 3, order });
	const got = await Subscribe.decode(
		new Reader(undefined, await encodeMessage(version, subscribe), version),
		version,
	);
	expect(got.order).toBe(order);

	const update = await encodeMessage(version, new SubscribeUpdate({ priority: 3, order, maxDelay: 250, ranges: [] }));
	// The length prefix, the priority byte, then the order.
	expect(update[1]).toBe(3 + 128);
	expect(update[2]).toBe(order === "ascending" ? 1 : 0);
	const decoded = await SubscribeUpdate.decode(new Reader(undefined, update, version), version);
	expect(decoded.order).toBe(order);

	const invalid = new Uint8Array(update);
	invalid[2] = 2;
	await expect(SubscribeUpdate.decode(new Reader(undefined, invalid, version), version)).rejects.toThrow(
		ProtocolViolation,
	);
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
	for (const live of [0, 250, Number.MAX_SAFE_INTEGER]) {
		const ranges: WireRange[] = [{ start: { live }, end: { time: 9n } }];
		const props = { id: 1n, broadcast: Path.from("room"), track: "video", priority: 3, ranges };
		const subscribe = await encodeMessage(version, new Subscribe(props));
		expect((await Subscribe.decode(new Reader(undefined, subscribe, version), version)).ranges).toEqual(ranges);
		const update = await encodeMessage(version, new SubscribeUpdate({ priority: 3, ranges }));
		expect((await SubscribeUpdate.decode(new Reader(undefined, update, version), version)).ranges).toEqual(ranges);
	}
	// The model rounds a lag up to whole milliseconds, as the wire carries it.
	expect(toWireRanges(rangesOf([{ start: { live: Duration.micros(1500) } }]))).toEqual([{ start: { live: 2 } }]);
	expect(requestedRanges({ startFrame: 0, ranges: [{ start: { live: 250 } }] })).toEqual([
		{ start: { live: Duration.millis(250) } },
	]);

	// No start on a wire without the Anchor, which carries only the span.
	const legacy = Version.DRAFT_06;
	const span = wireSpan([{ start: { live: Duration.millis(250) } }]);
	expect(span).toEqual({ start: undefined, end: undefined });
	const props = { id: 1n, broadcast: Path.from("room"), track: "video", priority: 3, startGroup: span.start };
	const old = await Subscribe.decode(
		new Reader(undefined, await encodeMessage(legacy, new Subscribe(props)), legacy),
		legacy,
	);
	expect(old.startGroup).toBeUndefined();

	// A live end is refused both ways.
	await expect(
		encodeMessage(version, new SubscribeUpdate({ priority: 3, ranges: [{ end: { live: 250 } as never }] })),
	).rejects.toThrow();
	// A range ending at time 250 has the shape of one starting live 250 ms back, but for the Anchor
	// Types: turn its end's Time (1) into Live (3).
	const timed = await encodeMessage(version, new SubscribeUpdate({ priority: 3, ranges: [{ end: { time: 250n } }] }));
	const live = await encodeMessage(version, new SubscribeUpdate({ priority: 3, ranges: [{ start: { live: 250 } }] }));
	const at = timed.findIndex((byte, index) => byte === 0 && live[index] === 3);
	expect(timed[at + 1]).toBe(1);
	const ended = Uint8Array.from(timed);
	ended[at + 1] = 3;
	await expect(SubscribeUpdate.decode(new Reader(undefined, ended, version), version)).rejects.toThrow(
		ProtocolViolation,
	);
});

// Lite-07 sends milliseconds plus one, 0 for no limit. Earlier wires send bare milliseconds, so
// no limit goes out as the largest value every peer reads, and lite-01/02 carry none at all.
test("max delay says no limit on lite-07, and the largest safe value before it", async () => {
	// After the size, the priority, and the order.
	const field = async (maxDelay: number | null) =>
		(await encodeMessage(Version.DRAFT_07, new SubscribeUpdate({ priority: 0, maxDelay, ranges: [] })))[3];
	expect([await field(null), await field(0), await field(5)]).toEqual([0, 1, 6]);

	for (const version of [Version.DRAFT_01, Version.DRAFT_03, Version.DRAFT_06, Version.DRAFT_07]) {
		for (const maxDelay of [null, 0, 250]) {
			const expected = {
				[Version.DRAFT_01]: null,
				[Version.DRAFT_03]: maxDelay ?? Number.MAX_SAFE_INTEGER,
				[Version.DRAFT_06]: maxDelay ?? Number.MAX_SAFE_INTEGER,
				[Version.DRAFT_07]: maxDelay,
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

test("optional subscriber milliseconds reject unsafe budgets", async () => {
	for (const maxDelay of [-1, 0.5, Number.MAX_SAFE_INTEGER + 1]) {
		await expect(
			encodeMessage(
				Version.DRAFT_07,
				new Subscribe({ id: 1n, broadcast: Path.from("room"), track: "video", priority: 3, maxDelay }),
			),
		).rejects.toThrow(RangeError);
		await expect(encodeMessage(Version.DRAFT_07, new SubscribeUpdate({ priority: 3, maxDelay }))).rejects.toThrow(
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
		expect(edit).toEqual({ remove, add, fillIds: [], probeIds: [] });
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

// An edit resolves to the Range IDs it names. Mirrors the Rust `lite07_an_edit_names_its_range_ids`.
test("lite-07 an edit names its range ids", () => {
	const live = new LiveRanges(Version.DRAFT_07, [{}]);
	const range = (group: bigint): WireRange => ({ start: { absolute: group } });
	const edit = (remove: bigint[], ranges: WireRange[]) =>
		live.apply(new SubscribeUpdate({ priority: 0, remove, ranges }));
	expect(edit([], [])).toEqual({ retired: [], added: [] });
	expect(edit([0n], [range(4n), range(5n)])).toEqual({
		retired: [0n],
		added: [
			[1n, range(4n)],
			[2n, range(5n)],
		],
	});
	expect(edit([1n], [range(6n)])).toEqual({ retired: [1n], added: [[3n, range(6n)]] });
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
	expect(live.edit(wide.slice(0, 2))).toEqual({ remove: [0n], add: wide.slice(0, 2), fillIds: [], probeIds: [] });
});

test("SUBSCRIBE_STARTED is type 4 on lite-07 and unknown before it", async () => {
	for (const start of [undefined, 0n, 7n]) {
		const started = new SubscribeStarted(4n, start);
		expect(await encode(Version.DRAFT_07, { started })).toEqual(
			new Uint8Array([4, 2, 4, start === undefined ? 0 : Number(start) + 1]),
		);
		const got = await responseRoundtrip(Version.DRAFT_07, { started });
		if (!("started" in got)) throw new Error("expected SUBSCRIBE_STARTED");
		expect([got.started.id, got.started.start]).toEqual([4n, start]);
	}
	await expect(encode(Version.DRAFT_06, { started: new SubscribeStarted(0n) })).rejects.toThrow();
	await expect(
		decodeSubscribeResponse(
			new Reader(undefined, new Uint8Array([4, 2, 0, 0]), Version.DRAFT_06),
			Version.DRAFT_06,
		),
	).rejects.toThrow(ProtocolViolation);
});

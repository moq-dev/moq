import { expect, test } from "bun:test";
import { ProtocolViolation } from "../error.ts";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import {
	decodeSubscribeResponse,
	emptyRange,
	encodeSubscribeResponse,
	exclusiveGroupEnd,
	inclusiveGroupEnd,
	Subscribe,
	SubscribeDrop,
	SubscribeEnd,
	SubscribeOk,
	type SubscribeResponse,
	SubscribeStart,
	SubscribeUpdate,
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
		ok: new SubscribeOk({ priority: 7, maxDelay: 250, startGroup: 3 }),
	});
	expect("ok" in got).toBe(true);
	if (!("ok" in got)) throw new Error("expected ok");
	expect(got.ok.priority).toBe(7);
	expect(got.ok.startGroup).toBe(3);
});

test("Subscribe round-trips every option including startGroup 0", async () => {
	const message = new Subscribe({
		id: 4n,
		broadcast: Path.from("test"),
		track: "video",
		priority: 7,
		maxDelay: 250,
		startGroup: 0,
		endGroup: 9,
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
	expect(got.endGroup).toBe(9);

	// Group 0 stays named when a Frame Start qualifies it: a subscription can resume
	// partway through group 0 (a catalog never leaves it).
	message.startFrame = 4;
	const resumed = await Subscribe.decode(
		new Reader(undefined, await encodeMessage(Version.DRAFT_06, message), Version.DRAFT_06),
		Version.DRAFT_06,
	);
	expect(resumed.startGroup).toBe(0);
	expect(resumed.startFrame).toBe(4);
	message.startFrame = 0;

	// A pre-06 wire encodes an explicit group 0 as sequence + 1, so it round-trips.
	// Omitting the floor is still the latest group, not group 0.
	const explicit = await Subscribe.decode(
		new Reader(undefined, await encodeMessage(Version.DRAFT_05, message), Version.DRAFT_05),
		Version.DRAFT_05,
	);
	expect(explicit.startGroup).toBe(0);
	expect(explicit.endGroup).toBe(9);
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
		startGroup: 0,
		endGroup: 12,
	});
	const got = await SubscribeUpdate.decode(
		new Reader(undefined, await encodeMessage(Version.DRAFT_06, message), Version.DRAFT_06),
		Version.DRAFT_06,
	);
	expect(got.priority).toBe(8);
	expect(got.maxDelay).toBe(500);
	expect(got.startGroup).toBeUndefined();
	expect(got.endGroup).toBe(12);

	// The same pre-06 encoding as SUBSCRIBE: explicit group 0 round-trips, and an
	// omitted floor stays omitted.
	const explicit = await SubscribeUpdate.decode(
		new Reader(undefined, await encodeMessage(Version.DRAFT_05, message), Version.DRAFT_05),
		Version.DRAFT_05,
	);
	expect(explicit.startGroup).toBe(0);
	expect(explicit.endGroup).toBe(12);
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
		startGroup: 3,
		startFrame: 2,
		endGroup: 9,
		endFrame: 4,
	});
	const decode = async (msg: Subscribe) =>
		Subscribe.decode(
			new Reader(undefined, await encodeMessage(Version.DRAFT_07, msg), Version.DRAFT_07),
			Version.DRAFT_07,
		);
	const got = await decode(message);
	expect([got.startGroup, got.startFrame, got.endGroup, got.endFrame]).toEqual([3, 2, 9, 4]);
	const whole = await decode(new Subscribe({ ...message, startFrame: 0, endFrame: undefined }));
	expect([whole.startGroup, whole.startFrame, whole.endGroup, whole.endFrame]).toEqual([3, 0, 9, undefined]);
	const open = await decode(
		new Subscribe({ ...message, startGroup: undefined, startFrame: 0, endGroup: undefined, endFrame: undefined }),
	);
	expect([open.startGroup, open.endGroup]).toEqual([undefined, undefined]);
});

test("a lite-07 list decodes as the span covering it", async () => {
	// Priority, order, and max delay as SubscribeUpdate writes them, then the ranges and
	// an absent wanted group.
	const head = (await encodeMessage(Version.DRAFT_07, new SubscribeUpdate({ priority: 0 }))).slice(1, 4);
	const update = (ranges: number[]) => {
		const body = [...head, ...ranges, 0];
		return SubscribeUpdate.decode(
			new Reader(undefined, new Uint8Array([body.length, ...body]), Version.DRAFT_07),
			Version.DRAFT_07,
		);
	};

	// Groups 2 through 4 and 7 through frame 1 of group 9.
	const sparse = await update([2, 1, 2, 0, 1, 5, 0, 1, 7, 0, 1, 9, 2]);
	expect([sparse.startGroup, sparse.startFrame, sparse.endGroup, sparse.endFrame]).toEqual([2, 0, 9, 1]);
	expect(sparse.ranges).toEqual([
		{ start: 2, end: 5 },
		{ start: 7, end: 10 },
	]);

	// Preserve that a relative anchor needs resolution before the publisher can serve it.
	const relative = await update([2, 2, 3, 1, 5, 0, 1, 7, 0, 1, 9, 0]);
	expect([relative.startGroup, relative.endGroup, relative.endFrame]).toEqual([undefined, 8, undefined]);
	expect(relative.ranges).toEqual([
		{ start: 0, end: 5, unresolved: true },
		{ start: 7, end: 9 },
	]);

	// No start reaches back to group 0; an empty range has no groups.
	const oldest = await update([2, 0, 1, 3, 0, 1, 5, 2, 1, 5, 2]);
	expect(oldest.ranges).toEqual([{ start: 0, end: 3 }, undefined]);

	await expect(update([0])).rejects.toThrow(ProtocolViolation);
	await expect(update([1, 4, 0, 0])).rejects.toThrow(ProtocolViolation);
	await expect(update([1, 0, 1, 0, 0])).rejects.toThrow(ProtocolViolation);
});

test("SubscribeStart round-trips on draft-05", async () => {
	const got = await responseRoundtrip(Version.DRAFT_05, { start: new SubscribeStart([42]) });
	expect("start" in got).toBe(true);
	if (!("start" in got)) throw new Error("expected start");
	expect(got.start.starts).toEqual([42]);
});

test("SubscribeStart carries the largest position on draft-07", async () => {
	// Type, length, largest group + 1, largest frame, one range start plus one.
	expect(await encode(Version.DRAFT_07, { start: new SubscribeStart([4], { group: 3, frame: 2 }) })).toEqual(
		new Uint8Array([0, 4, 4, 2, 1, 5]),
	);
	for (const largest of [undefined, { group: 3, frame: 2 }]) {
		const got = await responseRoundtrip(Version.DRAFT_07, { start: new SubscribeStart([4], largest) });
		if (!("start" in got)) throw new Error("expected start");
		expect(got.start.largest).toEqual(largest);
	}
	// Draft-06 has no largest position on the wire.
	expect(await encode(Version.DRAFT_06, { start: new SubscribeStart([4], { group: 3, frame: 2 }) })).toEqual(
		new Uint8Array([0, 1, 4]),
	);
});

test("SubscribeStart carries each range's start in place of one group on draft-07", async () => {
	const start = new SubscribeStart([2, undefined, 0]);
	// Type, length, no largest, three starts each plus one with 0 for none.
	expect(await encode(Version.DRAFT_07, { start })).toEqual(new Uint8Array([0, 5, 0, 3, 3, 0, 1]));
	const got = await responseRoundtrip(Version.DRAFT_07, { start });
	if (!("start" in got)) throw new Error("expected start");
	expect(got.start.starts).toEqual([2, undefined, 0]);
	// Draft-06's one `Group` names a single range.
	await expect(encode(Version.DRAFT_06, { start })).rejects.toThrow();
});

test("SubscribeEnd round-trips on draft-05", async () => {
	// Type, length, exclusive final group.
	expect(await encode(Version.DRAFT_05, { end: new SubscribeEnd(7) })).toEqual(new Uint8Array([1, 1, 7]));
	const got = await responseRoundtrip(Version.DRAFT_05, { end: new SubscribeEnd(7) });
	expect("end" in got).toBe(true);
	if (!("end" in got)) throw new Error("expected end");
	expect(got.end.group).toBe(7);
});

test("SubscribeEnd carries its error on draft-07", async () => {
	// Type, length, exclusive final group, then the error code + 1, 0 for a clean end.
	expect(await encode(Version.DRAFT_07, { end: new SubscribeEnd(7) })).toEqual(new Uint8Array([1, 2, 7, 0]));
	expect(await encode(Version.DRAFT_07, { end: new SubscribeEnd(7, 71) })).toEqual(new Uint8Array([1, 2, 7, 72]));
	const got = await responseRoundtrip(Version.DRAFT_07, { end: new SubscribeEnd(7, 71) });
	if (!("end" in got)) throw new Error("expected end");
	expect([got.end.group, got.end.error]).toEqual([7, 71]);
	// An older version has nowhere to carry it.
	await expect(encode(Version.DRAFT_06, { end: new SubscribeEnd(7, 71) })).rejects.toThrow();
});

test("SubscribeDrop is restored on draft-07", async () => {
	const drop: SubscribeResponse = { drop: new SubscribeDrop({ start: 1, end: 3, error: 7 }) };
	const wire = await encode(Version.DRAFT_07, drop);
	expect(wire).toEqual(new Uint8Array([2, 3, 1, 3, 7]));
	const got = await responseRoundtrip(Version.DRAFT_07, drop);
	if (!("drop" in got)) throw new Error("expected drop");
	expect([got.drop.start, got.drop.end, got.drop.error]).toEqual([1, 3, 7]);
});

test("SubscribeDrop is type 0x2 on draft-05 and 0x1 on draft-04", async () => {
	const drop: SubscribeResponse = { drop: new SubscribeDrop({ start: 1, end: 3, error: 0 }) };

	const wire05 = await encode(Version.DRAFT_05, drop);
	expect(wire05[0]).toBe(2);

	const wire04 = await encode(Version.DRAFT_04, drop);
	expect(wire04[0]).toBe(1);

	const got = await responseRoundtrip(Version.DRAFT_05, drop);
	expect("drop" in got).toBe(true);
	if (!("drop" in got)) throw new Error("expected drop");
	expect([got.drop.start, got.drop.end]).toEqual([1, 3]);
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

test("model and wire group ends convert without an off-by-one", () => {
	expect(exclusiveGroupEnd(undefined)).toBeUndefined();
	expect(exclusiveGroupEnd(0)).toBe(1);
	expect(exclusiveGroupEnd(9)).toBe(10);
	expect(inclusiveGroupEnd(undefined)).toBeUndefined();
	expect(inclusiveGroupEnd(1)).toBe(0);
	expect(inclusiveGroupEnd(10)).toBe(9);
	expect(() => inclusiveGroupEnd(0)).toThrow("empty subscription range cannot be encoded");
});

test("a requested range is empty when its bounds meet anywhere", () => {
	expect(emptyRange({})).toBe(false);
	expect(emptyRange({ endGroup: 0 })).toBe(true);
	expect(emptyRange({ startGroup: 5, endGroup: 5 })).toBe(true);
	expect(emptyRange({ startGroup: 6, endGroup: 5 })).toBe(true);
	expect(emptyRange({ startGroup: 5, endGroup: 6 })).toBe(false);
	expect(emptyRange({ startGroup: 5 })).toBe(false);
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

	const update = await encodeMessage(version, new SubscribeUpdate({ priority: 3, order, maxDelay: 250 }));
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

test("wanted group survives lite-07 and disappears on legacy wires", async () => {
	for (const version of [Version.DRAFT_05, Version.DRAFT_06, Version.DRAFT_07]) {
		for (const wantedGroup of [undefined, 0, 3, Number.MAX_SAFE_INTEGER]) {
			const update = new SubscribeUpdate({ priority: 7, wantedGroup });
			const encoded = await encodeMessage(version, update);
			const decoded = await SubscribeUpdate.decode(new Reader(undefined, encoded, version), version);
			expect(decoded.wantedGroup).toBe(version === Version.DRAFT_07 ? wantedGroup : undefined);
		}
	}
});

test("wanted groups refuse malformed input before writing bytes", async () => {
	for (const wantedGroup of [-1, 0.5, Number.MAX_SAFE_INTEGER + 1, Number.NaN]) {
		expect(() => new SubscribeUpdate({ priority: 0, wantedGroup })).toThrow(RangeError);
		const update = new SubscribeUpdate({ priority: 0 });
		update.wantedGroup = wantedGroup;
		await expect(encodeMessage(Version.DRAFT_07, update)).rejects.toThrow(RangeError);
	}
});

for (const version of [Version.DRAFT_05, Version.DRAFT_06, Version.DRAFT_07]) {
	test(`SUBSCRIBE_DROP refuses reversed ranges while retaining empty ranges on ${version}`, async () => {
		const malformed = new Reader(undefined, new Uint8Array([3, 3, 1, 0]), version);
		await expect(SubscribeDrop.decode(malformed)).rejects.toThrow("invalid dropped range");
		await expect(encode(version, { drop: new SubscribeDrop({ start: 3, end: 1, error: 0 }) })).rejects.toThrow();
		for (const [start, end] of [
			[2, 1],
			[Number.MAX_SAFE_INTEGER, Number.MAX_SAFE_INTEGER],
		]) {
			const got = await responseRoundtrip(version, { drop: new SubscribeDrop({ start, end, error: 0 }) });
			if (!("drop" in got)) throw new Error("missing drop");
			expect([got.drop.start, got.drop.end]).toEqual([start, end]);
		}
	});
}

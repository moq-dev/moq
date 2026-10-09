import { expect, test } from "bun:test";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import {
	decodeSubscribeResponse,
	EMPTY_START,
	emptyRange,
	encodeSubscribeResponse,
	exclusiveGroupEnd,
	inclusiveGroupEnd,
	type Start,
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

const LIVE: Start = { live: true, startFrame: 0 };
const floored = (startGroup: number, startFrame = 0): Start => ({ live: false, startGroup, startFrame });
const both = (startGroup: number, startFrame = 0): Start => ({ live: true, startGroup, startFrame });

/** What `start` reads back as on `version`, through SUBSCRIBE and SUBSCRIBE_UPDATE. */
async function roundtrip(version: Version, start: Start): Promise<Start> {
	const subscribe = new Subscribe({ id: 4n, broadcast: Path.from("test"), track: "video", priority: 7, start });
	const got = await Subscribe.decode(
		new Reader(undefined, await encodeMessage(version, subscribe), version),
		version,
	);
	const update = new SubscribeUpdate({ priority: 8, start });
	const updated = await SubscribeUpdate.decode(
		new Reader(undefined, await encodeMessage(version, update), version),
		version,
	);
	expect(updated.start).toEqual(got.start);
	return got.start;
}

test("Subscribe round-trips every option", async () => {
	const message = new Subscribe({
		id: 4n,
		broadcast: Path.from("test"),
		track: "video",
		priority: 7,
		maxDelay: 250,
		start: floored(3, 2),
		endGroup: 9,
		endFrame: 4,
	});
	for (const version of [Version.DRAFT_06, Version.DRAFT_07]) {
		const got = await Subscribe.decode(
			new Reader(undefined, await encodeMessage(version, message), version),
			version,
		);
		expect(got.priority).toBe(7);
		expect(got.maxDelay).toBe(250);
		expect(got.start).toEqual(floored(3, 2));
		expect(got.endGroup).toBe(9);
		expect(got.endFrame).toBe(4);
	}
});

test("lite-07 carries live beside the floor", async () => {
	for (const start of [LIVE, floored(0), floored(3, 5), both(0), both(4, 2)]) {
		expect(await roundtrip(Version.DRAFT_07, start)).toEqual(start);
	}

	// `Live`, then the floor's presence, then the floor itself, ahead of the two end varints;
	// the same bytes the Rust codec pins.
	const bytes = async (start: Start) =>
		Array.from(
			await encodeMessage(
				Version.DRAFT_07,
				new Subscribe({ id: 1n, broadcast: Path.from("room"), track: "video", priority: 0, start }),
			),
		);
	expect((await bytes(both(4, 2))).slice(-6)).toEqual([1, 1, 4, 2, 0, 0]);
	expect((await bytes(LIVE)).slice(-4)).toEqual([1, 0, 0, 0]);
});

test("lite-07 refuses neither live nor a floor", async () => {
	const neither = new Subscribe({
		id: 1n,
		broadcast: Path.from("room"),
		track: "video",
		priority: 0,
		start: { live: false, startFrame: 0 },
	});
	await expect(encodeMessage(Version.DRAFT_07, neither)).rejects.toThrow(EMPTY_START);

	// Clear `Live` on an encoded live-only SUBSCRIBE: the absent floor and the two end
	// varints follow it.
	const bytes = await encodeMessage(
		Version.DRAFT_07,
		new Subscribe({ id: 1n, broadcast: Path.from("room"), track: "video", priority: 0 }),
	);
	const live = bytes.length - 4;
	expect(bytes[live]).toBe(1);
	bytes[live] = 0;
	await expect(Subscribe.decode(new Reader(undefined, bytes, Version.DRAFT_07), Version.DRAFT_07)).rejects.toThrow(
		EMPTY_START,
	);
});

test("lite-06 folds live into group start zero", async () => {
	expect(await roundtrip(Version.DRAFT_06, LIVE)).toEqual(LIVE);
	// `live` with a floor goes out as (0, 0), and the receiver filters locally.
	expect(await roundtrip(Version.DRAFT_06, both(4))).toEqual(LIVE);
	expect(await roundtrip(Version.DRAFT_06, floored(7, 4))).toEqual(floored(7, 4));
	// A catalog resume partway through group 0 stays a floor.
	expect(await roundtrip(Version.DRAFT_06, floored(0, 4))).toEqual(floored(0, 4));
	// A floor of (0, 0) reads back as `live`, which lite-06 cannot tell apart.
	expect(await roundtrip(Version.DRAFT_06, floored(0))).toEqual(LIVE);
});

test("pre-06 folds live into an explicit group zero", async () => {
	for (const version of [Version.DRAFT_03, Version.DRAFT_04, Version.DRAFT_05]) {
		expect(await roundtrip(version, LIVE)).toEqual(LIVE);
		// Replay from the beginning, so the receiver can filter by age and floor.
		expect(await roundtrip(version, both(2))).toEqual(floored(0));
		expect(await roundtrip(version, floored(7))).toEqual(floored(7));
		// An explicit group 0 is 1 on the wire, not folded back to absent.
		expect(await roundtrip(version, floored(0))).toEqual(floored(0));
	}
});

test("SubscribeStart round-trips on draft-05", async () => {
	const got = await responseRoundtrip(Version.DRAFT_05, { start: new SubscribeStart(42) });
	expect("start" in got).toBe(true);
	if (!("start" in got)) throw new Error("expected start");
	expect(got.start.group).toBe(42);
});

test("SubscribeStart carries the largest position on draft-07", async () => {
	// Type, length, group, largest group + 1, largest frame.
	expect(await encode(Version.DRAFT_07, { start: new SubscribeStart(4, { group: 3, frame: 2 }) })).toEqual(
		new Uint8Array([0, 3, 4, 4, 2]),
	);
	for (const largest of [undefined, { group: 3, frame: 2 }]) {
		const got = await responseRoundtrip(Version.DRAFT_07, { start: new SubscribeStart(4, largest) });
		if (!("start" in got)) throw new Error("expected start");
		expect([got.start.group, got.start.largest]).toEqual([4, largest]);
	}
	// Draft-06 has no largest position on the wire.
	expect(await encode(Version.DRAFT_06, { start: new SubscribeStart(4, { group: 3, frame: 2 }) })).toEqual(
		new Uint8Array([0, 1, 4]),
	);
});

test("SubscribeEnd round-trips on draft-05", async () => {
	// Type, length, group: no stream count before draft-07.
	expect(await encode(Version.DRAFT_05, { end: new SubscribeEnd(7, 3) })).toEqual(new Uint8Array([1, 1, 7]));
	const got = await responseRoundtrip(Version.DRAFT_05, { end: new SubscribeEnd(7, 3) });
	expect("end" in got).toBe(true);
	if (!("end" in got)) throw new Error("expected end");
	expect([got.end.group, got.end.streams]).toEqual([7, 0]);
});

test("SubscribeEnd carries the stream count on draft-07", async () => {
	expect(await encode(Version.DRAFT_07, { end: new SubscribeEnd(7, 3) })).toEqual(new Uint8Array([1, 2, 7, 3]));
	const got = await responseRoundtrip(Version.DRAFT_07, { end: new SubscribeEnd(7, 3) });
	if (!("end" in got)) throw new Error("expected end");
	expect([got.end.group, got.end.streams]).toEqual([7, 3]);
});

test("SubscribeDrop is gone on draft-07", async () => {
	const drop: SubscribeResponse = { drop: new SubscribeDrop({ start: 1, end: 3, error: 0 }) };
	await expect(encode(Version.DRAFT_07, drop)).rejects.toThrow();

	// A draft-06 DROP is an unknown response type on draft-07.
	const wire06 = await encode(Version.DRAFT_06, drop);
	await expect(
		decodeSubscribeResponse(new Reader(undefined, wire06, Version.DRAFT_07), Version.DRAFT_07),
	).rejects.toThrow("unknown subscribe response type: 2");
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
	// The live edge may sit below any floor, so only an end of 0 empties a live range.
	expect(emptyRange({ live: true, startGroup: 5, endGroup: 5 })).toBe(false);
	expect(emptyRange({ live: true, startGroup: 5, endGroup: 0 })).toBe(true);
});

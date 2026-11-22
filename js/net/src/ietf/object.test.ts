import { expect, test } from "bun:test";
import { FrameTooLarge, ProtocolViolation, StreamCode, Stream as StreamError } from "../error.ts";
import { Cursor, concat, encodeVarint, Reader, Writer } from "../stream.ts";
import { Timescale, Timestamp } from "../time.ts";
import { MAX_FRAME_BYTES } from "../util/limits.ts";
import { FetchHeader } from "./fetch.ts";
import { decodeFetchObject, decodeGroupGap, EndOfRange, encodeGroupGap, Frame, Group, objectTime } from "./object.ts";
import { Version } from "./version.ts";

const VERSION = Version.DRAFT_18;

/** A Properties block of `values`, each a varint. */
function block(...values: (number | bigint)[]): Uint8Array {
	return concat(values.map((value) => encodeVarint(value, VERSION)));
}

function readGap(properties: Uint8Array): bigint | undefined {
	return new Cursor(properties, VERSION).exact(properties.byteLength, decodeGroupGap);
}

test("a Prior Group ID Gap round trips", () => {
	for (const gap of [1n, 63n, 64n, 1n << 40n]) expect(readGap(encodeGroupGap(gap, VERSION))).toBe(gap);
});

test("a Prior Group ID Gap is read among other properties, a Timestamp only skipped", () => {
	// A Timestamp (0x10) of 5, then the gap, its type a delta from 0x10.
	expect(readGap(block(0x10, 5, 0x3c - 0x10, 7))).toBe(7n);
	expect(readGap(block(0x10, 5))).toBeUndefined();
	// Inside Immutable Properties (0x0b).
	const sealed = block(0x3c, 3);
	expect(readGap(concat([block(0x0b, sealed.byteLength), sealed]))).toBe(3n);
});

test("a repeated Prior Group ID Gap is a malformed track", () => {
	const sealed = block(0x3c, 3);
	for (const properties of [
		block(0x3c, 2, 0, 2),
		concat([block(0x0b, sealed.byteLength), sealed, block(0x3c - 0x0b, 3)]),
	]) {
		let caught: unknown;
		try {
			readGap(properties);
		} catch (err) {
			caught = err;
		}
		expect(caught).toBeInstanceOf(StreamError);
		expect((caught as StreamError).code).toBe(StreamCode.MalformedTrack);
	}
});

test("a received object's time is its group's key", () => {
	expect(objectTime(Timescale.MILLI, 1234n)).toEqual(Timestamp.fromMillis(1234));
	expect(objectTime(undefined, 1234n)).toEqual(Timestamp.unscaled(1234n));
	// A key past what a Timestamp holds at a scale refuses the group; unscaled, any key is fine.
	const huge = BigInt(Number.MAX_SAFE_INTEGER) + 1n;
	expect(() => objectTime(Timescale.MILLI, huge)).toThrow(RangeError);
	expect(objectTime(undefined, huge).key).toBe(huge);
});

test("an object's header goes out in one write, ahead of its payload", async () => {
	const chunks: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void chunks.push(new Uint8Array(chunk)) }),
		VERSION,
	);
	const flags = {
		hasExtensions: true,
		hasSubgroup: false,
		hasSubgroupObject: false,
		hasEnd: true,
		hasPriority: true,
		firstObject: true,
	};
	// A length past a one-byte varint.
	const payload = new Uint8Array(300).fill(7);
	await new Frame({ payload, groupGap: 3n }).encode(writer, flags, VERSION);
	// A status object has no payload, so it is the header alone.
	await new Frame({ endOfTrack: true }).encode(writer, flags, VERSION);
	expect(chunks.length).toBe(3);
	expect(chunks[1]).toEqual(payload);

	const c = new Cursor(concat(chunks), VERSION);
	const frame = Frame.decode(c, flags);
	expect(frame.payload).toEqual(payload);
	expect(frame.groupGap).toBe(3n);
	expect(Frame.decode(c, flags).endOfTrack).toBe(true);
});

test("an object or fetched object declaring more than 32 MiB is refused at its header", () => {
	const flags = {
		hasExtensions: false,
		hasSubgroup: false,
		hasSubgroupObject: false,
		hasEnd: false,
		hasPriority: false,
		firstObject: true,
	};
	// Only the header: the cap must not wait for any payload bytes.
	const object = (size: number) =>
		new Cursor(concat([encodeVarint(0, VERSION), encodeVarint(size, VERSION)]), VERSION);
	expect(Frame.decode(object(MAX_FRAME_BYTES), flags).streamed).toBe(MAX_FRAME_BYTES);
	expect(() => Frame.decode(object(MAX_FRAME_BYTES + 1), flags)).toThrow(FrameTooLarge);

	// Flags 0: subgroup zero and no optional fields, so the length follows them directly.
	const fetched = (size: number) =>
		new Cursor(concat([encodeVarint(0, VERSION), encodeVarint(size, VERSION)]), VERSION);
	expect(decodeFetchObject(fetched(MAX_FRAME_BYTES))).toMatchObject({ kind: "object", size: MAX_FRAME_BYTES });
	expect(() => decodeFetchObject(fetched(MAX_FRAME_BYTES + 1))).toThrow(FrameTooLarge);
});

test("a data stream's header goes out in one write, stream type included", async () => {
	for (const version of [Version.DRAFT_22]) {
		const chunks: Uint8Array[] = [];
		const writer = new Writer(
			new WritableStream<Uint8Array>({ write: (chunk) => void chunks.push(new Uint8Array(chunk)) }),
			version,
		);

		// Every optional field present, with values past a one-byte varint.
		const group = new Group({
			trackAlias: 1_000_000n,
			groupId: 70_000n,
			subGroupId: 300n,
			publisherPriority: 200,
			flags: {
				hasExtensions: true,
				hasSubgroup: true,
				hasSubgroupObject: false,
				hasEnd: true,
				hasPriority: true,
				firstObject: true,
			},
		});
		await group.encode(writer, version);
		await new FetchHeader({ requestId: 1_000_000n }).encode(writer, version);
		expect(chunks.length).toBe(2);

		const decoded = await Group.decode(new Reader(undefined, chunks[0], version), version);
		expect(decoded).toEqual(group);

		const fetch = new Reader(undefined, chunks[1], version);
		expect(await fetch.u53()).toBe(FetchHeader.type);
		expect((await FetchHeader.decode(fetch, version)).requestId).toBe(1_000_000n);
		expect(await fetch.done()).toBe(true);
	}
});

test("an End of Range decodes a 62-bit Object ID and refuses unknown flags", () => {
	const cursor = (...values: (number | bigint)[]) => new Cursor(block(...values), VERSION);
	const max = (1n << 62n) - 1n;
	expect(decodeFetchObject(cursor(EndOfRange.NonExistent, max, max))).toEqual({
		kind: "endOfRange",
		reason: EndOfRange.NonExistent,
		group: max,
		object: max,
	});
	for (const flags of [0x80, 0x10d, 1n << 53n, max]) {
		expect(() => decodeFetchObject(cursor(flags, 0, 0))).toThrow(ProtocolViolation);
	}
});

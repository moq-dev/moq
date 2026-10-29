import { expect, test } from "bun:test";
import { Cursor, concat, Reader, Writer } from "../stream.ts";
import { Timescale, Timestamp } from "../time.ts";
import { FetchHeader } from "./fetch.ts";
import { decodeObjectTime, encodeObjectExtensions, Frame, Group } from "./object.ts";
import { Version } from "./version.ts";

const VERSION = Version.DRAFT_18;

/** `timestamp` written at `timescale` and read back. */
function roundtrip(timestamp: Timestamp, timescale: Timescale): Timestamp | undefined {
	const properties = encodeObjectExtensions(timestamp, timescale, VERSION);
	return decodeObjectTime(new Cursor(properties, VERSION), timescale);
}

test("a same-scale timestamp is written exactly, up to the largest a Timestamp holds", () => {
	for (const value of [0, 1, 9_007_199_254_740_985, Number.MAX_SAFE_INTEGER]) {
		const read = roundtrip(new Timestamp(value, Timescale.NANO), Timescale.NANO);
		expect(read?.value).toBe(value);
	}
});

test("a timestamp is converted to the track's scale", () => {
	expect(roundtrip(Timestamp.fromMillis(1234), Timescale.NANO)?.value).toBe(1_234_000_000);
	expect(roundtrip(Timestamp.fromMicros(1_500), Timescale.MILLI)?.value).toBe(2);
});

test("a conversion past what a Timestamp holds is refused before anything is written", () => {
	// 10^10 ms is 10^16 ns, past 2^53.
	expect(() => encodeObjectExtensions(Timestamp.fromMillis(10_000_000_000), Timescale.NANO, VERSION)).toThrow(
		RangeError,
	);
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
	await new Frame({ payload, timestamp: Timestamp.fromMillis(1234) }).encode(writer, flags, Timescale.MILLI, VERSION);
	// A status object has no payload, so it is the header alone.
	await new Frame({ endOfTrack: true }).encode(writer, flags, Timescale.MILLI, VERSION);
	expect(chunks.length).toBe(3);
	expect(chunks[1]).toEqual(payload);

	const c = new Cursor(concat(chunks), VERSION);
	const frame = Frame.decode(c, flags, Timescale.MILLI);
	expect(frame.payload).toEqual(payload);
	expect(frame.timestamp?.as(Timescale.MILLI)).toBe(1234);
	expect(Frame.decode(c, flags, Timescale.MILLI).endOfTrack).toBe(true);
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
			groupId: 70_000,
			subGroupId: 300,
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

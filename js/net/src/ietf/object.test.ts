import { expect, test } from "bun:test";
import { Cursor, Reader, Writer } from "../stream.ts";
import { Timescale, Timestamp } from "../time.ts";
import { FetchHeader } from "./fetch.ts";
import { decodeObjectTime, encodeObjectExtensions, Group } from "./object.ts";
import { Version } from "./version.ts";

const VERSION = Version.DRAFT_17;

/** `timestamp` written at `timescale` and read back. */
async function roundtrip(timestamp: Timestamp, timescale: Timescale): Promise<Timestamp | undefined> {
	const properties = await encodeObjectExtensions(timestamp, timescale, VERSION);
	return decodeObjectTime(new Cursor(properties, VERSION), timescale);
}

test("a same-scale timestamp is written exactly, up to the largest a Timestamp holds", async () => {
	for (const value of [0, 1, 9_007_199_254_740_985, Number.MAX_SAFE_INTEGER]) {
		const read = await roundtrip(new Timestamp(value, Timescale.NANO), Timescale.NANO);
		expect(read?.value).toBe(value);
	}
});

test("a timestamp is converted to the track's scale", async () => {
	expect((await roundtrip(Timestamp.fromMillis(1234), Timescale.NANO))?.value).toBe(1_234_000_000);
	expect((await roundtrip(Timestamp.fromMicros(1_500), Timescale.MILLI))?.value).toBe(2);
});

test("a conversion past what a Timestamp holds is refused before anything is written", async () => {
	// 10^10 ms is 10^16 ns, past 2^53.
	await expect(encodeObjectExtensions(Timestamp.fromMillis(10_000_000_000), Timescale.NANO, VERSION)).rejects.toThrow(
		RangeError,
	);
});

test("a data stream's header goes out in one write, stream type included", async () => {
	for (const version of [Version.DRAFT_14, Version.DRAFT_22]) {
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

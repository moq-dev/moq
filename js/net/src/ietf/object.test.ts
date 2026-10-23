import { expect, test } from "bun:test";
import { Cursor } from "../stream.ts";
import { Timescale, Timestamp } from "../time.ts";
import { decodeObjectTime, encodeObjectExtensions } from "./object.ts";
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

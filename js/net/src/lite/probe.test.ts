import { expect, test } from "bun:test";
import { Reader, Writer } from "../stream.ts";
import { Probe } from "./probe.ts";
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

async function bytes(f: (w: Writer) => Promise<void>): Promise<Uint8Array> {
	const written: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void written.push(new Uint8Array(chunk)) }),
	);
	await f(writer);
	writer.close();
	await writer.closed;
	return concat(written);
}

async function roundTrip(msg: Probe, version: Version = Version.DRAFT_05): Promise<Probe> {
	const reader = new Reader(undefined, await bytes((w) => msg.encode(w, version)));
	const got = await Probe.decode(reader, version);
	expect(await reader.done()).toBe(true);
	return got;
}

// Both fields travel as 0 for unknown, independently, so a transport exposing only
// one still has a legal message to send.
test("each field is independently unknown", async () => {
	for (const [bitrate, rtt] of [
		[1_000_000, 40],
		[undefined, 40],
		[1_000_000, undefined],
		[undefined, undefined],
	] as const) {
		const got = await roundTrip(new Probe({ bitrate, rtt }));
		expect(got.bitrate).toBe(bitrate);
		expect(got.rtt).toBe(rtt);
	}
});

// A measured zero is indistinguishable from unknown on the wire, so it rounds up to
// the smallest value that still reads as a measurement.
test("a measured zero rounds up", async () => {
	const got = await roundTrip(new Probe({ bitrate: 0, rtt: 0 }));
	expect(got.bitrate).toBe(1);
	expect(got.rtt).toBe(1);
});

// lite-03 predates the RTT field; the bitrate half still round-trips.
test("lite-03 carries the bitrate alone", async () => {
	const got = await roundTrip(new Probe({ bitrate: 1_000_000, rtt: 40 }), Version.DRAFT_03);
	expect(got.bitrate).toBe(1_000_000);
	expect(got.rtt).toBeUndefined();
});

// `smoothedRtt` is a DOMHighResTimeStamp, so a real browser hands us a fraction. The
// varint encoder converts with `BigInt`, which throws on one, and the publisher's
// catch would then close the probe stream for the rest of the session.
test("a fractional RTT must not reach the encoder", async () => {
	await expect(bytes((w) => new Probe({ bitrate: 1_000, rtt: 12.34 }).encode(w, Version.DRAFT_05))).rejects.toThrow();

	// Rounding first is what the publisher does, and it encodes cleanly.
	const got = await roundTrip(new Probe({ bitrate: 1_000, rtt: Math.round(12.34) }));
	expect(got.rtt).toBe(12);
});

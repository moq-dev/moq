import { expect, test } from "bun:test";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import { Track, TrackInfo } from "./track.ts";
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

test("TrackInfo round-trips on draft-05", async () => {
	const info = new TrackInfo({
		priority: 7,
		ordered: false,
		timescale: 90000,
		compress: true,
	});
	const reader = new Reader(undefined, await bytes((w) => info.encode(w, Version.DRAFT_05_WIP)));
	const got = await TrackInfo.decode(reader, Version.DRAFT_05_WIP);
	expect(got.priority).toBe(7);
	expect(got.ordered).toBe(false);
	expect(got.timescale).toBe(90000);
	expect(got.compress).toBe(true);
});

test("TrackInfo compress hint is additive", async () => {
	// Reserved wire values (>1) decode as the boolean hint `true`. Hand-frame the
	// body and prefix it with the Message size, the way the encoder does.
	const body = await bytes(async (w) => {
		await w.u8(7);
		await w.bool(false);
		await w.u53(90000);
		await w.u53(9); // reserved compress value
	});
	const framed = await bytes(async (w) => {
		await w.u53(body.byteLength);
		await w.write(body);
	});
	const got = await TrackInfo.decode(new Reader(undefined, framed), Version.DRAFT_05_WIP);
	expect(got.compress).toBe(true);
});

test("Track request round-trips on draft-05", async () => {
	const msg = new Track(Path.from("room"), "video");
	const reader = new Reader(undefined, await bytes((w) => msg.encode(w, Version.DRAFT_05_WIP)));
	const got = await Track.decode(reader, Version.DRAFT_05_WIP);
	expect(got.broadcast).toBe(Path.from("room"));
	expect(got.track).toBe("video");
});

test("TRACK_INFO is rejected before draft-05", async () => {
	const info = new TrackInfo({ compress: false });
	await expect(bytes((w) => info.encode(w, Version.DRAFT_04))).rejects.toThrow();
});

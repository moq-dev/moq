import { expect, test } from "bun:test";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import { infoDefaults } from "../track.ts";
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
		cache: 2000,
		timescale: 90000,
	});
	const reader = new Reader(undefined, await bytes((w) => info.encode(w, Version.DRAFT_05)));
	const got = await TrackInfo.decode(reader, Version.DRAFT_05);
	expect(got.priority).toBe(7);
	expect(got.ordered).toBe(false);
	expect(got.cache).toBe(2000);
	expect(got.timescale).toBe(90000);
});

test("TrackInfo defaults match cross-language wire bytes", async () => {
	const info = new TrackInfo(infoDefaults());
	expect(await bytes((w) => info.encode(w, Version.DRAFT_05))).toEqual(
		new Uint8Array([0x06, 0x00, 0x00, 0x53, 0x88, 0x43, 0xe8]),
	);
});

test("Track request round-trips on draft-05", async () => {
	const msg = new Track(Path.from("room"), "video");
	const reader = new Reader(undefined, await bytes((w) => msg.encode(w, Version.DRAFT_05)));
	const got = await Track.decode(reader, Version.DRAFT_05);
	expect(got.broadcast).toBe(Path.from("room"));
	expect(got.track).toBe("video");
});

test("TRACK_INFO is rejected before draft-05", async () => {
	const info = new TrackInfo({ timescale: 90000 });
	await expect(bytes((w) => info.encode(w, Version.DRAFT_04))).rejects.toThrow();
});

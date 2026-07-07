import { expect, test } from "bun:test";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import { AnnounceBroadcast } from "./announce.ts";
import { OriginSchema } from "./origin.ts";
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

async function roundTrip(msg: AnnounceBroadcast, version: Version): Promise<AnnounceBroadcast> {
	const reader = new Reader(undefined, await bytes((w) => msg.encode(w, version)));
	return AnnounceBroadcast.decode(reader, version);
}

test("AnnounceBroadcast round-trips on draft-05", async () => {
	const hops = [OriginSchema.parse(7n)];
	const active = new AnnounceBroadcast({ suffix: Path.from("room/cam"), active: true, hops });
	const gotActive = await roundTrip(active, Version.DRAFT_05);
	expect(gotActive.active).toBe(true);
	expect(gotActive.suffix).toBe(Path.from("room/cam"));
	expect(gotActive.hops).toEqual(hops);

	const ended = new AnnounceBroadcast({ suffix: Path.from("room/cam"), active: false });
	const gotEnded = await roundTrip(ended, Version.DRAFT_05);
	expect(gotEnded.active).toBe(false);
	expect(gotEnded.suffix).toBe(Path.from("room/cam"));
});

test("AnnounceBroadcast accepts explicit restart status on draft-05", async () => {
	const wire = await bytes((w) =>
		new AnnounceBroadcast({ suffix: Path.from("room/cam"), active: true }).encode(w, Version.DRAFT_05),
	);
	wire[1] = 2;

	const got = await AnnounceBroadcast.decode(new Reader(undefined, wire), Version.DRAFT_05);
	expect(got.active).toBe(true);
	expect(got.suffix).toBe(Path.from("room/cam"));
});

test("AnnounceBroadcast rejects explicit restart status before draft-05", async () => {
	const wire = await bytes((w) =>
		new AnnounceBroadcast({ suffix: Path.from("room/cam"), active: true }).encode(w, Version.DRAFT_04),
	);
	wire[1] = 2;

	await expect(AnnounceBroadcast.decode(new Reader(undefined, wire), Version.DRAFT_04)).rejects.toThrow();
});

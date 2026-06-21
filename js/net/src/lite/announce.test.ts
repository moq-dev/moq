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

test("AnnounceBroadcast epoch round-trips on draft-05", async () => {
	const hops = [OriginSchema.parse(7n)];
	// 1_700_000_000_000 is ~ms since 2020 in 2023; the others probe the edges of u53.
	for (const epoch of [0, 1, 1_700_000_000_000, 2 ** 52]) {
		const active = new AnnounceBroadcast({ suffix: Path.from("room/cam"), active: true, epoch, hops });
		const gotActive = await roundTrip(active, Version.DRAFT_05_WIP);
		expect(gotActive.active).toBe(true);
		expect(gotActive.epoch).toBe(epoch);
		expect(gotActive.suffix).toBe(Path.from("room/cam"));
		expect(gotActive.hops).toEqual(hops);

		const ended = new AnnounceBroadcast({ suffix: Path.from("room/cam"), active: false, epoch });
		const gotEnded = await roundTrip(ended, Version.DRAFT_05_WIP);
		expect(gotEnded.active).toBe(false);
		expect(gotEnded.epoch).toBe(epoch);
	}
});

test("AnnounceBroadcast epoch is omitted before draft-05", async () => {
	// Pre-lite-05 carries no epoch on the wire, so a nonzero epoch decodes back as 0.
	const msg = new AnnounceBroadcast({
		suffix: Path.from("room/cam"),
		active: true,
		epoch: 42,
		hops: [OriginSchema.parse(7n)],
	});
	const got = await roundTrip(msg, Version.DRAFT_04);
	expect(got.epoch).toBe(0);
	expect(got.suffix).toBe(Path.from("room/cam"));
});

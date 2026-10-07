import { expect, test } from "bun:test";
import * as Epoch from "../epoch.ts";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import { decodeAnnounceBroadcast, encodeAnnounceBroadcast } from "./announce.ts";
import { Fetch } from "./fetch.ts";
import { Subscribe } from "./subscribe.ts";
import { Track } from "./track.ts";
import { Version } from "./version.ts";

async function wire(f: (w: Writer) => Promise<void>, version: Version): Promise<Reader> {
	const chunks: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void chunks.push(new Uint8Array(chunk)) }),
		version,
	);
	await f(writer);
	writer.close();
	await writer.closed;
	const out = new Uint8Array(chunks.reduce((sum, c) => sum + c.byteLength, 0));
	let offset = 0;
	for (const c of chunks) {
		out.set(c, offset);
		offset += c.byteLength;
	}
	return new Reader(undefined, out, version);
}

const room = Path.from("room");

// Each message carries the epoch on draft-07 and drops it on draft-06, which has no room.
for (const [version, expected] of [
	[Version.DRAFT_07, true],
	[Version.DRAFT_06, false],
] as const) {
	test(`ANNOUNCE_START, TRACK, SUBSCRIBE and FETCH ${expected ? "carry" : "drop"} the epoch on ${version.toString(16)}`, async () => {
		const epoch = Epoch.mint();
		const want = expected ? epoch : undefined;

		const announce = await decodeAnnounceBroadcast(
			await wire(
				(w) => encodeAnnounceBroadcast(w, { status: "active", suffix: room, epoch, hops: [] }, version),
				version,
			),
			version,
		);
		expect(announce).toMatchObject({ status: "active", epoch: want });

		const track = await Track.decode(
			await wire((w) => new Track(room, "video", epoch).encode(w, version), version),
			version,
		);
		expect(track.epoch).toBe(want);

		const subscribe = new Subscribe({ id: 0n, broadcast: room, epoch, track: "video", priority: 0 });
		const decoded = await Subscribe.decode(await wire((w) => subscribe.encode(w, version), version), version);
		expect(decoded.epoch).toBe(want);

		const fetch = new Fetch({ broadcast: room, epoch, track: "video", priority: 0, group: 0 });
		expect((await Fetch.decode(await wire((w) => fetch.encode(w, version), version), version)).epoch).toBe(want);
	});
}

test("an absent epoch round-trips as none on draft-07", async () => {
	const decoded = await Track.decode(
		await wire((w) => new Track(room, "video").encode(w, Version.DRAFT_07), Version.DRAFT_07),
		Version.DRAFT_07,
	);
	expect(decoded.epoch).toBeUndefined();
});

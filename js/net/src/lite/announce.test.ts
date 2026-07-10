import { expect, test } from "bun:test";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import { type AnnounceBroadcast, decodeAnnounceBroadcast, encodeAnnounceBroadcast } from "./announce.ts";
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
	const reader = new Reader(undefined, await bytes((w) => encodeAnnounceBroadcast(w, msg, version)));
	return decodeAnnounceBroadcast(reader, version);
}

test("AnnounceBroadcast round-trips on draft-05", async () => {
	const hops = [OriginSchema.parse(7n)];
	const gotActive = await roundTrip({ status: "active", suffix: Path.from("room/cam"), hops }, Version.DRAFT_05);
	expect(gotActive).toEqual({ status: "active", suffix: Path.from("room/cam"), hops });

	const gotEnded = await roundTrip({ status: "ended", suffix: Path.from("room/cam") }, Version.DRAFT_05);
	expect(gotEnded).toEqual({ status: "ended", suffix: Path.from("room/cam") });
});

test("AnnounceBroadcast round-trips on draft-06", async () => {
	const hops = [OriginSchema.parse(7n)];
	const gotActive = await roundTrip({ status: "active", suffix: Path.from("room/cam"), hops }, Version.DRAFT_06);
	expect(gotActive).toEqual({ status: "active", suffix: Path.from("room/cam"), hops });

	const gotEnded = await roundTrip({ status: "endedId", id: 3n }, Version.DRAFT_06);
	expect(gotEnded).toEqual({ status: "endedId", id: 3n });

	const gotRestart = await roundTrip({ status: "restart", id: 3n, hops }, Version.DRAFT_06);
	expect(gotRestart).toEqual({ status: "restart", id: 3n, hops });
});

test("AnnounceBroadcast rejects cross-version forms", async () => {
	await expect(
		bytes((w) => encodeAnnounceBroadcast(w, { status: "endedId", id: 1n }, Version.DRAFT_05)),
	).rejects.toThrow();
	await expect(
		bytes((w) => encodeAnnounceBroadcast(w, { status: "restart", id: 1n, hops: [] }, Version.DRAFT_05)),
	).rejects.toThrow();
	await expect(
		bytes((w) => encodeAnnounceBroadcast(w, { status: "ended", suffix: Path.from("room/cam") }, Version.DRAFT_06)),
	).rejects.toThrow();
});

test("AnnounceBroadcast accepts explicit restart status on draft-05", async () => {
	const wire = await bytes((w) =>
		encodeAnnounceBroadcast(w, { status: "active", suffix: Path.from("room/cam"), hops: [] }, Version.DRAFT_05),
	);
	wire[1] = 2;

	const got = await decodeAnnounceBroadcast(new Reader(undefined, wire), Version.DRAFT_05);
	expect(got).toEqual({ status: "active", suffix: Path.from("room/cam"), hops: [] });
});

test("AnnounceBroadcast rejects explicit restart status before draft-05", async () => {
	const wire = await bytes((w) =>
		encodeAnnounceBroadcast(w, { status: "active", suffix: Path.from("room/cam"), hops: [] }, Version.DRAFT_04),
	);
	wire[1] = 2;

	await expect(decodeAnnounceBroadcast(new Reader(undefined, wire), Version.DRAFT_04)).rejects.toThrow();
});

import { expect, test } from "bun:test";
import { ProtocolViolation } from "../error.ts";
import { OriginSchema } from "../hop.ts";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import {
	type AnnounceBroadcast,
	AnnounceRequest,
	decodeAnnounceBroadcast,
	encodeAnnounceBroadcast,
} from "./announce.ts";
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
	// An absent cost encodes as zero and decodes explicitly.
	const gotActive = await roundTrip({ status: "active", suffix: Path.from("room/cam"), hops }, Version.DRAFT_06);
	expect(gotActive).toEqual({ status: "active", suffix: Path.from("room/cam"), hops, cost: { warm: 0n, cold: 0n } });

	// Asymmetric on purpose: the two magnitudes travel independently, so a swapped
	// or shared encode would round-trip a symmetric pair unnoticed.
	const cost = { warm: 12n, cold: 30n };
	const gotCost = await roundTrip({ status: "active", suffix: Path.from("room/cam"), hops, cost }, Version.DRAFT_06);
	expect(gotCost).toEqual({ status: "active", suffix: Path.from("room/cam"), hops, cost });

	const gotEnded = await roundTrip({ status: "endedId", id: 3n }, Version.DRAFT_06);
	expect(gotEnded).toEqual({ status: "endedId", id: 3n });

	const gotRestart = await roundTrip({ status: "restart", id: 3n, hops, cost }, Version.DRAFT_06);
	expect(gotRestart).toEqual({ status: "restart", id: 3n, hops, cost });
});

test("AnnounceBroadcast drops the route cost before draft-06", async () => {
	// Pre-lite-06 has no room for a cost on the wire, so one set locally is
	// simply not sent, keeping mixed-version meshes ranking on hop count.
	const got = await roundTrip(
		{ status: "active", suffix: Path.from("room/cam"), hops: [], cost: { warm: 9n, cold: 9n } },
		Version.DRAFT_05,
	);
	expect(got).toEqual({ status: "active", suffix: Path.from("room/cam"), hops: [], cost: undefined });
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

async function requestRoundTrip(msg: AnnounceRequest, version: Version): Promise<AnnounceRequest> {
	const reader = new Reader(undefined, await bytes((w) => msg.encode(w, version)));
	return AnnounceRequest.decode(reader, version);
}

// Draft04/05 carry the subscriber's origin id so the publisher can skip reflected
// announces before they hit the wire.
test("AnnounceRequest carries excludeHop on draft-05", async () => {
	const got = await requestRoundTrip(new AnnounceRequest(Path.from("room/"), 42n), Version.DRAFT_05);
	expect(got.excludeHop).toBe(42n);
});

// Draft06 dropped the field: the receiver's reflected-announce check catches the same
// loops, so a value set locally is simply not sent and decodes as zero.
test("AnnounceRequest drops excludeHop on draft-06", async () => {
	const msg = new AnnounceRequest(Path.from("room/"), 42n);
	const got = await requestRoundTrip(msg, Version.DRAFT_06);
	expect(got.excludeHop).toBe(0n);

	const with05 = await bytes((w) => msg.encode(w, Version.DRAFT_05));
	const with06 = await bytes((w) => msg.encode(w, Version.DRAFT_06));
	expect(with06.byteLength).toBeLessThan(with05.byteLength);
});

test("a hop chain that revisits a hop is refused in both directions", async () => {
	const four = OriginSchema.parse(4n);
	const eight = OriginSchema.parse(8n);
	const looped: AnnounceBroadcast = { status: "active", suffix: Path.from("room"), hops: [four, eight, four] };

	// Outbound: refused before it reaches the wire. A receiver must close the session over
	// a repeated Hop ID, so sending one costs someone else their session.
	await expect(bytes((w) => encodeAnnounceBroadcast(w, looped, Version.DRAFT_06))).rejects.toThrow("appears twice");

	// Inbound: encode a chain that is legal, then rewrite its last hop to repeat the
	// first. Only a non-conforming sender produces these bytes, which is why they have to
	// be built by hand. Every id here is a one-byte varint, so the length is unchanged.
	const legal: AnnounceBroadcast = {
		status: "active",
		suffix: Path.from("room"),
		hops: [four, eight, OriginSchema.parse(9n)],
	};
	const forged = await bytes((w) => encodeAnnounceBroadcast(w, legal, Version.DRAFT_06));
	const nine = forged.lastIndexOf(9);
	expect(nine).toBeGreaterThan(0);
	forged[nine] = 4;

	// The type carries the consequence, not just the text: the subscriber's dispatch closes
	// the session on `instanceof ProtocolViolation`, so a plain Error here would reset the
	// stream and leave a nonconforming peer free to repeat itself.
	await expect(decodeAnnounceBroadcast(new Reader(undefined, forged), Version.DRAFT_06)).rejects.toThrow(
		ProtocolViolation,
	);
	await expect(decodeAnnounceBroadcast(new Reader(undefined, forged), Version.DRAFT_06)).rejects.toThrow(
		"appears twice",
	);

	// Repeated unknowns are not a loop: 0 identifies nothing, so any number of hops may
	// be unknown. A lite-03 announcement is nothing but these.
	const unknown = OriginSchema.parse(0n);
	const anonymous: AnnounceBroadcast = {
		status: "active",
		suffix: Path.from("room"),
		hops: [unknown, four, unknown],
	};
	expect(await roundTrip(anonymous, Version.DRAFT_05)).toEqual(anonymous);
});

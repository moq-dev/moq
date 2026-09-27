import { expect, test } from "bun:test";
import { ProtocolViolation } from "../error.ts";
import { HopSchema, UNKNOWN_HOP } from "../hop.ts";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import {
	type AnnounceBroadcast,
	AnnounceHistory,
	AnnounceOk,
	AnnounceRequest,
	decodeAnnounceBroadcast,
	decodeAnnounceBroadcastMaybe,
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
	const hops = [HopSchema.parse(7n)];
	const gotActive = await roundTrip({ status: "active", suffix: Path.from("room/cam"), hops }, Version.DRAFT_05);
	expect(gotActive).toEqual({ status: "active", suffix: Path.from("room/cam"), hops });

	const gotEnded = await roundTrip({ status: "ended", suffix: Path.from("room/cam") }, Version.DRAFT_05);
	expect(gotEnded).toEqual({ status: "ended", suffix: Path.from("room/cam") });
});

test("AnnounceBroadcast round-trips on draft-06", async () => {
	const hops = [HopSchema.parse(7n)];
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

test("AnnounceBroadcast skips an unknown type on draft-06", async () => {
	const encoded = await bytes(async (w) => {
		await w.u53(4);
		await w.u53(1);
		await w.u8(0);
	});
	const reader = new Reader(undefined, encoded);
	expect(await decodeAnnounceBroadcast(reader, Version.DRAFT_06)).toEqual({ status: "skipped" });
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

// Draft04/05 carry the subscriber's Hop ID so the publisher can skip reflected
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

// Draft07 carries the hidden opt-in; every earlier version decodes as not opted in.
test("AnnounceRequest carries hidden from draft-07", async () => {
	for (const hidden of [false, true]) {
		const msg = new AnnounceRequest(Path.from("room/"), 0n, hidden);
		expect((await requestRoundTrip(msg, Version.DRAFT_07)).hidden).toBe(hidden);
		expect((await requestRoundTrip(msg, Version.DRAFT_06)).hidden).toBe(false);
	}
});

// The draft reserves Hop ID 0 for a responder that was never assigned an id, or that
// withholds it to obscure its routing. Rejecting it tore down the announce stream of a
// conforming publisher.
test("AnnounceOk accepts the reserved unknown origin", async () => {
	const msg = new AnnounceOk(UNKNOWN_HOP, 3);
	const reader = new Reader(undefined, await bytes((w) => msg.encode(w, Version.DRAFT_05)));
	const got = await AnnounceOk.decode(reader, Version.DRAFT_05);
	expect(got.hop).toBe(UNKNOWN_HOP);
	expect(got.active).toBe(3);
});

test("AnnounceOk round-trips a declared origin", async () => {
	const msg = new AnnounceOk(HopSchema.parse(42n), 1);
	const reader = new Reader(undefined, await bytes((w) => msg.encode(w, Version.DRAFT_05)));
	const got = await AnnounceOk.decode(reader, Version.DRAFT_05);
	expect(got.hop).toBe(HopSchema.parse(42n));
	expect(got.active).toBe(1);
});

test("a hop chain that revisits a hop is refused in both directions", async () => {
	const four = HopSchema.parse(4n);
	const eight = HopSchema.parse(8n);
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
		hops: [four, eight, HopSchema.parse(9n)],
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
	const unknown = HopSchema.parse(0n);
	const anonymous: AnnounceBroadcast = {
		status: "active",
		suffix: Path.from("room"),
		hops: [unknown, four, unknown],
	};
	expect(await roundTrip(anonymous, Version.DRAFT_05)).toEqual(anonymous);
});

function hex(data: Uint8Array): string {
	return Array.from(data, (b) => b.toString(16).padStart(2, "0")).join("");
}

function unhex(text: string): Uint8Array {
	return new Uint8Array(text.match(/../g)?.map((b) => Number.parseInt(b, 16)) ?? []);
}

// Resolve every announcement on a lite-07 stream, as the subscriber does.
async function resolveStream(data: Uint8Array) {
	const reader = new Reader(undefined, data);
	const history = new AnnounceHistory();
	const out: unknown[] = [];
	for (;;) {
		const msg = await decodeAnnounceBroadcastMaybe(reader, Version.DRAFT_07);
		if (!msg) return out;
		switch (msg.status) {
			case "active":
				out.push({ start: history.start(msg) });
				break;
			case "restart":
				out.push({ update: msg.id, hops: history.update(msg).hops });
				break;
			case "endedId":
				out.push({ end: msg.id, suffix: history.end(msg.id) });
				break;
			default:
				throw new Error(`unexpected ${msg.status}`);
		}
	}
}

const hop = (id: bigint) => HopSchema.parse(id);
const relay = hop(0x2222n);
const GOLDEN_RESOLVED = [
	{ start: { suffix: Path.from("room/a/cam"), hops: [hop(0x1111n), relay] } },
	{ start: { suffix: Path.from("room/a/mic"), hops: [hop(0x3333n), relay] } },
	{ update: 0n, hops: [hop(0x4444n), relay] },
	{ end: 1n, suffix: Path.from("room/a/mic") },
	{ start: { suffix: Path.from("room/b"), hops: [hop(0x5555n), relay] } },
];

// Pinned from the Rust encoder (`lite::compress::tests::golden_stream_is_pinned`), so the
// JS decoder is checked against real compressed output.
const GOLDEN =
	"001600000a726f6f6d2f612f63616d000251116222000000000d0102036d696301017333010000020a00010180004444010000010101000d02010162020180005555010000";

test("AnnounceHistory resolves the Rust encoder's compressed stream", async () => {
	expect(await resolveStream(unhex(GOLDEN))).toEqual(GOLDEN_RESOLVED);
});

// JS always encodes literally; Rust decodes these bytes too (`js_literal_stream_decodes`).
const JS_LITERAL =
	"001600000a726f6f6d2f612f63616d000251116222000000001600000a726f6f6d2f612f6d6963000273336222000000020c0000028000444462220000000101010014000006726f6f6d2f620002800055556222000000";

test("the literal draft-07 stream matches what Rust decodes", async () => {
	const wire = await bytes(async (w) => {
		const v = Version.DRAFT_07;
		const cost = { warm: 0n, cold: 0n };
		await encodeAnnounceBroadcast(
			w,
			{ status: "active", suffix: Path.from("room/a/cam"), hops: [hop(0x1111n), relay], cost },
			v,
		);
		await encodeAnnounceBroadcast(
			w,
			{ status: "active", suffix: Path.from("room/a/mic"), hops: [hop(0x3333n), relay], cost },
			v,
		);
		await encodeAnnounceBroadcast(w, { status: "restart", id: 0n, hops: [hop(0x4444n), relay], cost }, v);
		await encodeAnnounceBroadcast(w, { status: "endedId", id: 1n }, v);
		await encodeAnnounceBroadcast(
			w,
			{ status: "active", suffix: Path.from("room/b"), hops: [hop(0x5555n), relay], cost },
			v,
		);
	});
	expect(hex(wire)).toBe(JS_LITERAL);
	expect(await resolveStream(wire)).toEqual(GOLDEN_RESOLVED);
});

test("AnnounceHistory rejects a base that is not live", () => {
	const history = new AnnounceHistory();
	const base = { distance: 1n, keep: 0 };
	// A new stream has nothing to copy from.
	expect(() => history.start({ suffix: Path.from("a"), hops: [], pathBase: base })).toThrow(ProtocolViolation);

	const fresh = new AnnounceHistory();
	fresh.start({ suffix: Path.from("a/b"), hops: [hop(1n)] });
	fresh.start({ suffix: Path.from("c"), hops: [] });
	fresh.end(0n);
	// Id 0 is two back, and retired.
	expect(() => fresh.start({ suffix: Path.from("x"), hops: [], pathBase: { distance: 2n, keep: 1 } })).toThrow(
		ProtocolViolation,
	);
});

test("AnnounceHistory rejects a keep longer than the base", () => {
	const history = new AnnounceHistory();
	history.start({ suffix: Path.from("a/b"), hops: [hop(1n)] });
	expect(() => history.start({ suffix: Path.from("x"), hops: [], pathBase: { distance: 1n, keep: 3 } })).toThrow(
		ProtocolViolation,
	);
	expect(() => history.update({ id: 0n, hops: [], hopBase: { distance: 1n, keep: 2 } })).toThrow(ProtocolViolation);
});

test("AnnounceHistory rejects a resolved chain that repeats a hop", () => {
	const history = new AnnounceHistory();
	history.start({ suffix: Path.from("a"), hops: [hop(1n), hop(2n)] });
	expect(() =>
		history.start({ suffix: Path.from("b"), hops: [hop(2n)], hopBase: { distance: 1n, keep: 1 } }),
	).toThrow(ProtocolViolation);
});

test("a keep without a base is a violation on draft-07", async () => {
	// ANNOUNCE_START: type, length, path base 0, path keep 1, empty suffix, empty hops, cost.
	const wire = await bytes(async (w) => {
		await w.u53(0);
		await w.u53(8);
		for (const b of [0, 1, 0, 0, 0, 0, 0, 0]) await w.u8(b);
	});
	await expect(decodeAnnounceBroadcast(new Reader(undefined, wire), Version.DRAFT_07)).rejects.toThrow(
		ProtocolViolation,
	);
});

test("draft-06 has no room for a base", async () => {
	const msg: AnnounceBroadcast = {
		status: "active",
		suffix: Path.from("x"),
		hops: [],
		pathBase: { distance: 1n, keep: 1 },
	};
	await expect(bytes((w) => encodeAnnounceBroadcast(w, msg, Version.DRAFT_06))).rejects.toThrow();
});

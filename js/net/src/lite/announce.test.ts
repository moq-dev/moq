import { expect, test } from "bun:test";
import { ProtocolViolation } from "../error.ts";
import { Cost, toHop, UNKNOWN_HOP } from "../hop.ts";
import * as Instance from "../instance.ts";
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
	type Offer,
	ROUTE_MAX_HOPS,
	requestFits,
	requestParts,
} from "./announce.ts";
import { MAX_MESSAGE_SIZE } from "./message.ts";
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

async function bytes(f: (w: Writer) => Promise<void>, version: Version): Promise<Uint8Array> {
	const written: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void written.push(new Uint8Array(chunk)) }),
		version,
	);
	await f(writer);
	writer.close();
	await writer.closed;
	return concat(written);
}

async function roundTrip(msg: AnnounceBroadcast, version: Version): Promise<AnnounceBroadcast> {
	const reader = new Reader(
		undefined,
		await bytes((w) => encodeAnnounceBroadcast(w, msg, version), version),
		version,
	);
	return decodeAnnounceBroadcast(reader, version);
}

test("AnnounceBroadcast round-trips on draft-05", async () => {
	const hops = [toHop(7n)];
	const gotActive = await roundTrip({ status: "active", suffix: Path.from("room/cam"), hops }, Version.DRAFT_05);
	expect(gotActive).toEqual({ status: "active", suffix: Path.from("room/cam"), hops });

	const gotEnded = await roundTrip({ status: "ended", suffix: Path.from("room/cam") }, Version.DRAFT_05);
	expect(gotEnded).toEqual({ status: "ended", suffix: Path.from("room/cam") });
});

test("AnnounceBroadcast round-trips on draft-06", async () => {
	const hops = [toHop(7n)];
	// An absent cost encodes as zero and decodes explicitly.
	const gotActive = await roundTrip({ status: "active", suffix: Path.from("room/cam"), hops }, Version.DRAFT_06);
	expect(gotActive).toEqual({ status: "active", suffix: Path.from("room/cam"), hops, cost: 0n });

	// A priced start round-trips its one static cost.
	const cost = 12n;
	const gotCost = await roundTrip({ status: "active", suffix: Path.from("room/cam"), hops, cost }, Version.DRAFT_06);
	expect(gotCost).toEqual({ status: "active", suffix: Path.from("room/cam"), hops, cost });

	const gotEnded = await roundTrip({ status: "endedId", id: 3n }, Version.DRAFT_06);
	expect(gotEnded).toEqual({ status: "endedId", id: 3n });

	const gotUpdate = await roundTrip({ status: "update", id: 3n, hops, cost }, Version.DRAFT_06);
	expect(gotUpdate).toEqual({ status: "update", id: 3n, hops, cost });
});

test("AnnounceBroadcast carries a restart only on draft-07", async () => {
	const instance = Instance.random();
	for (const msg of [
		{ status: "restart" as const, id: 3n, instance, epoch: 0n, cost: 12n },
		{ status: "restart" as const, id: 3n, instance, epoch: 7n, cost: 12n },
	]) {
		expect(await roundTrip(msg, Version.DRAFT_07)).toEqual(msg);
	}

	// Older versions send an end and a start instead, and skip the type as unknown.
	await expect(
		bytes((w) => encodeAnnounceBroadcast(w, { status: "restart", id: 1n }, Version.DRAFT_06), Version.DRAFT_06),
	).rejects.toThrow();
	const encoded = await bytes(async (w) => {
		await w.u53(3);
		await w.u53(1);
		await w.u8(1);
	}, Version.DRAFT_06);
	const reader = new Reader(undefined, encoded, Version.DRAFT_06);
	expect(await decodeAnnounceBroadcast(reader, Version.DRAFT_06)).toEqual({ status: "skipped" });
});

test("AnnounceBroadcast skips an unknown type on draft-06", async () => {
	const encoded = await bytes(async (w) => {
		await w.u53(4);
		await w.u53(1);
		await w.u8(0);
	}, Version.DRAFT_06);
	const reader = new Reader(undefined, encoded, Version.DRAFT_06);
	expect(await decodeAnnounceBroadcast(reader, Version.DRAFT_06)).toEqual({ status: "skipped" });
});

// Type 5 was a lite-07 message once; the version is unpublished, so it is now unknown there too.
test("AnnounceBroadcast skips type 5 as unknown on draft-07", async () => {
	const encoded = await bytes(async (w) => {
		await w.u53(5);
		await w.u53(2);
		await w.u8(0);
		await w.u8(0);
	}, Version.DRAFT_07);
	const reader = new Reader(undefined, encoded, Version.DRAFT_07);
	expect(await decodeAnnounceBroadcast(reader, Version.DRAFT_07)).toEqual({ status: "skipped" });
});

test("AnnounceBroadcast drops the route cost before draft-06", async () => {
	// Pre-lite-06 has no room for a cost on the wire, so one set locally is
	// simply not sent, keeping mixed-version meshes ranking on hop count.
	const got = await roundTrip(
		{ status: "active", suffix: Path.from("room/cam"), hops: [], cost: 9n },
		Version.DRAFT_05,
	);
	expect(got).toEqual({ status: "active", suffix: Path.from("room/cam"), hops: [], cost: undefined });
});

test("AnnounceBroadcast rejects cross-version forms", async () => {
	await expect(
		bytes((w) => encodeAnnounceBroadcast(w, { status: "endedId", id: 1n }, Version.DRAFT_05), Version.DRAFT_05),
	).rejects.toThrow();
	await expect(
		bytes(
			(w) => encodeAnnounceBroadcast(w, { status: "update", id: 1n, hops: [] }, Version.DRAFT_05),
			Version.DRAFT_05,
		),
	).rejects.toThrow();
	await expect(
		bytes(
			(w) => encodeAnnounceBroadcast(w, { status: "ended", suffix: Path.from("room/cam") }, Version.DRAFT_06),
			Version.DRAFT_06,
		),
	).rejects.toThrow();
});

test("AnnounceBroadcast accepts explicit restart status on draft-05", async () => {
	const wire = await bytes(
		(w) =>
			encodeAnnounceBroadcast(w, { status: "active", suffix: Path.from("room/cam"), hops: [] }, Version.DRAFT_05),
		Version.DRAFT_05,
	);
	wire[1] = 2;

	const got = await decodeAnnounceBroadcast(new Reader(undefined, wire, Version.DRAFT_05), Version.DRAFT_05);
	expect(got).toEqual({ status: "active", suffix: Path.from("room/cam"), hops: [] });
});

test("AnnounceBroadcast rejects explicit restart status before draft-05", async () => {
	const wire = await bytes(
		(w) =>
			encodeAnnounceBroadcast(w, { status: "active", suffix: Path.from("room/cam"), hops: [] }, Version.DRAFT_04),
		Version.DRAFT_04,
	);
	wire[1] = 2;

	await expect(
		decodeAnnounceBroadcast(new Reader(undefined, wire, Version.DRAFT_04), Version.DRAFT_04),
	).rejects.toThrow();
});

async function requestRoundTrip(msg: AnnounceRequest, version: Version): Promise<AnnounceRequest> {
	const reader = new Reader(undefined, await bytes((w) => msg.encode(w, version), version), version);
	return AnnounceRequest.decode(reader, version);
}

// Draft04/05 carry the subscriber's Hop ID so the publisher can skip reflected
// announces before they hit the wire.
test("AnnounceRequest carries excludeHop on draft-05", async () => {
	const got = await requestRoundTrip(new AnnounceRequest(Path.from("room"), toHop(42n)), Version.DRAFT_05);
	expect(got.excludeHop).toBe(toHop(42n));
});

// Draft06 dropped the field: the receiver's reflected-announce check catches the same
// loops, so a value set locally is simply not sent and decodes as zero.
test("AnnounceRequest drops excludeHop on draft-06", async () => {
	const msg = new AnnounceRequest(Path.from("room"), toHop(42n));
	const got = await requestRoundTrip(msg, Version.DRAFT_06);
	expect(got.excludeHop).toBe(UNKNOWN_HOP);

	const with05 = await bytes((w) => msg.encode(w, Version.DRAFT_05), Version.DRAFT_05);
	const with06 = await bytes((w) => msg.encode(w, Version.DRAFT_06), Version.DRAFT_06);
	expect(with06.byteLength).toBeLessThan(with05.byteLength);
});

// Lite-07 asks on its Announce Stream: whether the edit continues, the Prefix IDs to remove, then
// each prefix to add with its hidden opt-in. The stream-opening request is every earlier version's.
test("ANNOUNCE_REQUEST edits the requested prefixes on lite-07", async () => {
	const v = Version.DRAFT_07;
	const msg: AnnounceBroadcast = {
		status: "request",
		more: true,
		remove: [1n, 2n],
		add: [
			{ prefix: Path.from("room"), hidden: true },
			{ prefix: Path.empty(), hidden: false },
		],
	};
	const wire = await bytes((w) => encodeAnnounceBroadcast(w, msg, v), v);
	expect(wire).toEqual(new Uint8Array([16, 13, 1, 2, 1, 2, 2, 4, 0x72, 0x6f, 0x6f, 0x6d, 1, 0, 0]));
	expect(await roundTrip(msg, v)).toEqual(msg);
	const empty: AnnounceBroadcast = { status: "request", more: false, remove: [], add: [] };
	expect(await bytes((w) => encodeAnnounceBroadcast(w, empty, v), v)).toEqual(new Uint8Array([16, 3, 0, 0, 0]));
	await expect(roundTrip(msg, Version.DRAFT_06)).rejects.toThrow();
	await expect(bytes((w) => new AnnounceRequest(Path.from("room"), UNKNOWN_HOP).encode(w, v), v)).rejects.toThrow();

	// A count the body cannot hold, and a More or a Hidden that is no boolean, are malformed.
	const decode = (body: number[]) =>
		decodeAnnounceBroadcast(new Reader(undefined, new Uint8Array([16, body.length, ...body]), v), v);
	await expect(decode([0, 3, 1, 0])).rejects.toThrow();
	await expect(decode([0, 0, 1, 0, 2])).rejects.toThrow();
	await expect(decode([2, 0, 0])).rejects.toThrow();
});

// The requested set is its prefixes as an ANNOUNCE_REQUEST adding them all would carry them,
// which fits one message.
test("a requested set fits one message", () => {
	const v = Version.DRAFT_07;
	const long = (i: number) => Path.from(`${String(i).padStart(4, "0")}/${"x".repeat(1000)}`);
	const fits = Array.from({ length: 64 }, (_, i) => long(i));
	expect(requestFits(fits, v)).toBe(true);
	expect(requestFits([...fits, long(64), long(65)], v)).toBe(false);
	expect(requestFits([], v)).toBe(true);
});

// An edit too large for one message goes in as many as it takes, each fitting one, every one but
// the last continuing it, and together holding the whole edit in order, as `request_parts` does.
test("an edit too large for one message goes in parts", async () => {
	const v = Version.DRAFT_07;
	// Prefix IDs from 2^28 take five bytes.
	const remove = Array.from({ length: 20_000 }, (_, i) => (1n << 28n) + BigInt(i));
	const add = Array.from({ length: 3000 }, (_, i) => ({
		prefix: Path.from(String(i).padStart(20, "0")),
		hidden: i % 2 === 0,
	}));
	const parts = requestParts(remove, add, v);
	expect(parts.length).toBeGreaterThan(2);
	const removed: bigint[] = [];
	const added: { prefix: Path.Valid; hidden: boolean }[] = [];
	for (const [i, part] of parts.entries()) {
		expect(part.more).toBe(i + 1 < parts.length);
		const wire = await bytes((w) => encodeAnnounceBroadcast(w, part, v), v);
		// The type and the length take at most 4 bytes.
		expect(wire.byteLength).toBeLessThanOrEqual(MAX_MESSAGE_SIZE + 4);
		expect(await roundTrip(part, v)).toEqual(part);
		removed.push(...part.remove);
		added.push(...part.add);
	}
	expect(removed).toEqual(remove);
	expect(added).toEqual(add);

	// An edit that fits goes whole, and one with nothing in it still goes.
	expect(requestParts([1n], [{ prefix: Path.from("a"), hidden: false }], v).map(({ more }) => more)).toEqual([false]);
	expect(requestParts([], [], v)).toEqual([{ status: "request", more: false, remove: [], add: [] }]);
});

// The draft reserves Hop ID 0 for a responder that was never assigned an id, or that
// withholds it to obscure its routing. Rejecting it tore down the announce stream of a
// conforming publisher.
test("AnnounceOk accepts the reserved unknown origin", async () => {
	const msg = new AnnounceOk(UNKNOWN_HOP, 3);
	const reader = new Reader(
		undefined,
		await bytes((w) => msg.encode(w, Version.DRAFT_05), Version.DRAFT_05),
		Version.DRAFT_05,
	);
	const got = await AnnounceOk.decode(reader, Version.DRAFT_05);
	expect(got.hop).toBe(UNKNOWN_HOP);
	expect(got.active).toBe(3);
});

test("AnnounceOk round-trips a declared origin", async () => {
	const msg = new AnnounceOk(toHop(42n), 1);
	const reader = new Reader(
		undefined,
		await bytes((w) => msg.encode(w, Version.DRAFT_05), Version.DRAFT_05),
		Version.DRAFT_05,
	);
	const got = await AnnounceOk.decode(reader, Version.DRAFT_05);
	expect(got.hop).toBe(toHop(42n));
	expect(got.active).toBe(1);
});

// A QUIC varint stops at 2^62-1, so draft-06 carries a wider hop as unknown.
test("a 64-bit hop is unknown on draft-06", async () => {
	const version = Version.DRAFT_06;
	const wide = toHop((1n << 64n) - 1n);
	const got = await roundTrip({ status: "active", suffix: Path.from("room"), hops: [wide] }, version);
	expect(got).toMatchObject({ hops: [UNKNOWN_HOP] });

	const msg = new AnnounceOk(wide, 0);
	const reader = new Reader(undefined, await bytes((w) => msg.encode(w, version), version), version);
	expect((await AnnounceOk.decode(reader, version)).hop).toBe(UNKNOWN_HOP);
});

test("a hop chain that revisits a hop is refused in both directions", async () => {
	const four = toHop(4n);
	const eight = toHop(8n);
	const looped: AnnounceBroadcast = { status: "active", suffix: Path.from("room"), hops: [four, eight, four] };

	// Outbound: refused before it reaches the wire. A receiver must close the session over
	// a repeated Hop ID, so sending one costs someone else their session.
	await expect(bytes((w) => encodeAnnounceBroadcast(w, looped, Version.DRAFT_06), Version.DRAFT_06)).rejects.toThrow(
		"appears twice",
	);

	// Inbound: encode a chain that is legal, then rewrite its last hop to repeat the
	// first. Only a non-conforming sender produces these bytes, which is why they have to
	// be built by hand. Every id here is a one-byte varint, so the length is unchanged.
	const legal: AnnounceBroadcast = {
		status: "active",
		suffix: Path.from("room"),
		hops: [four, eight, toHop(9n)],
	};
	const forged = await bytes((w) => encodeAnnounceBroadcast(w, legal, Version.DRAFT_06), Version.DRAFT_06);
	const nine = forged.lastIndexOf(9);
	expect(nine).toBeGreaterThan(0);
	forged[nine] = 4;

	// The type carries the consequence, not just the text: the subscriber's dispatch closes
	// the session on `instanceof ProtocolViolation`, so a plain Error here would reset the
	// stream and leave a nonconforming peer free to repeat itself.
	await expect(
		decodeAnnounceBroadcast(new Reader(undefined, forged, Version.DRAFT_06), Version.DRAFT_06),
	).rejects.toThrow(ProtocolViolation);
	await expect(
		decodeAnnounceBroadcast(new Reader(undefined, forged, Version.DRAFT_06), Version.DRAFT_06),
	).rejects.toThrow("appears twice");

	// Repeated unknowns are not a loop: 0 identifies nothing, so any number of hops may
	// be unknown. A lite-03 announcement is nothing but these.
	const unknown = toHop(0n);
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
	const reader = new Reader(undefined, data, Version.DRAFT_07);
	const history = new AnnounceHistory();
	const out: unknown[] = [];
	for (;;) {
		const msg = await decodeAnnounceBroadcastMaybe(reader, Version.DRAFT_07);
		if (!msg) return out;
		switch (msg.status) {
			case "active": {
				const { route, instance, epoch, cost } = msg;
				out.push({ start: history.start(msg), route, instance, epoch, cost });
				break;
			}
			case "update":
				history.get(msg.id);
				out.push({ update: msg.id, cost: msg.cost });
				break;
			case "endedId":
				out.push({ end: msg.id, suffix: history.end(msg.id) });
				break;
			default:
				throw new Error(`unexpected ${msg.status}`);
		}
	}
}

const a = Instance.parse("a");
const b = Instance.parse("b");
const claim = { route: 0n, instance: a, epoch: 0n, cost: 0n };
const GOLDEN_RESOLVED = [
	{ start: Path.from("room/a/cam"), ...claim },
	{ start: Path.from("room/a/mic"), ...claim },
	{ update: 0n, cost: 0n },
	{ end: 1n, suffix: Path.from("room/a/mic") },
	{ start: Path.from("room/b"), ...claim },
];

// Rust's compressed output, pinned by `lite::compress::tests::golden_stream_is_pinned`.
const GOLDEN =
	"001300000a726f6f6d2f612f63616d000161000000000c0102036d696300016100000002020000010101000a02010162000161000000";

test("AnnounceHistory resolves the Rust encoder's compressed stream", async () => {
	expect(await resolveStream(unhex(GOLDEN))).toEqual(GOLDEN_RESOLVED);
});

// JS always encodes literally. Rust's `js_literal_stream_decodes` pins the same bytes.
const JS_LITERAL =
	"001300000a726f6f6d2f612f63616d000161000000001300000a726f6f6d2f612f6d6963000161000000020200000101010" +
	"00f000006726f6f6d2f62000161000000";

test("the literal draft-07 stream matches what Rust decodes", async () => {
	const wire = await bytes(async (w) => {
		const v = Version.DRAFT_07;
		const start = (suffix: string): AnnounceBroadcast => ({
			status: "active",
			suffix: Path.from(suffix),
			route: 0n,
			instance: a,
			epoch: 0n,
			cost: 0n,
		});
		await encodeAnnounceBroadcast(w, start("room/a/cam"), v);
		await encodeAnnounceBroadcast(w, start("room/a/mic"), v);
		await encodeAnnounceBroadcast(w, { status: "update", id: 0n, cost: 0n }, v);
		await encodeAnnounceBroadcast(w, { status: "endedId", id: 1n }, v);
		await encodeAnnounceBroadcast(w, start("room/b"), v);
	}, Version.DRAFT_07);
	expect(hex(wire)).toBe(JS_LITERAL);
	expect(await resolveStream(wire)).toEqual(GOLDEN_RESOLVED);
});

// `rs/moq-net/fixtures/lite.json` pins the same bytes on both sides.
test("the route layer round-trips on draft-07", async () => {
	const v = Version.DRAFT_07;
	const offer: Offer = {
		path: [
			{ node: 7n, session: 0n },
			{ node: 9n, session: 2n },
		],
		draining: false,
		priority: 0,
		metric: 5n,
		downOnly: true,
	};
	const one: Offer = { ...offer, path: [{ node: 7n, session: 0n }], downOnly: false, metric: 0n };
	const cases: [AnnounceBroadcast, number[]][] = [
		// The path first, the publisher first, then flags, priority (offset by 128), and metric.
		[
			{ status: "routeStart", node: 0x1234n, prefix: 3n, suffix: Path.empty(), exact: false, offer },
			[6, 13, 0x92, 0x34, 3, 0, 0, 2, 7, 0, 9, 2, 1, 128, 5],
		],
		// Exact: a route at the requested prefix whose own prefix it is.
		[
			{ status: "routeStart", node: 7n, prefix: 0n, suffix: Path.empty(), exact: true, offer: one },
			[6, 10, 7, 0, 0, 1, 1, 7, 0, 0, 128, 0],
		],
		// Flag 0x2: a node on the path drains.
		[
			{
				status: "routeUpdate",
				id: 1n,
				offer: {
					...offer,
					path: [{ node: 7n, session: 0n }],
					draining: true,
					priority: -3,
					downOnly: false,
				},
			},
			[7, 7, 1, 1, 7, 0, 2, 125, 5],
		],
		// As many hops as a route may carry.
		[
			{
				status: "routeUpdate",
				id: 1n,
				offer: {
					...one,
					path: Array.from({ length: ROUTE_MAX_HOPS }, (_, i) => ({ node: BigInt(i + 1), session: 0n })),
				},
			},
			[
				7,
				5 + 2 * ROUTE_MAX_HOPS,
				1,
				ROUTE_MAX_HOPS,
				...Array.from({ length: ROUTE_MAX_HOPS }, (_, i) => [i + 1, 0]).flat(),
				0,
				128,
				0,
			],
		],
		[{ status: "routeHold", id: 1n }, [0xb, 1, 1]],
		[{ status: "routeEnd", id: 1n }, [8, 1, 1]],
		[{ status: "dead", dead: { kind: "drain", node: 7n } }, [15, 2, 1, 7]],
		// Each moved offer is sized, so a receiver refusing one still reads the next.
		[
			{
				status: "dead",
				dead: {
					kind: "closed",
					hop: { node: 7n, session: 3n },
					far: 9n,
					gone: false,
					moved: 1n,
				},
			},
			[15, 6, 0, 7, 3, 9, 0, 1],
		],
		// The routes it counts follow as ROUTE_UPDATEs of their own.
		[{ status: "routeUpdate", id: 1n, offer: one }, [7, 7, 1, 1, 7, 0, 0, 128, 0]],
		[
			{
				status: "dead",
				dead: { kind: "closed", hop: { node: 7n, session: 3n }, far: 9n, gone: true, moved: 0n },
			},
			[15, 6, 0, 7, 3, 9, 1, 0],
		],
		// Revision 4 travels as 5: 0 certifies nothing.
		[{ status: "floor", id: 1n, revision: 4n }, [9, 2, 1, 5]],
		[{ status: "floor", id: 1n, revision: undefined }, [9, 2, 1, 0]],
	];
	for (const [msg, wire] of cases) {
		const encoded = await bytes((w) => encodeAnnounceBroadcast(w, msg, v), v);
		expect([...encoded]).toEqual(wire);
		expect(await roundTrip(msg, v)).toEqual(msg);
		// Older versions have none of it.
		await expect(
			bytes((w) => encodeAnnounceBroadcast(w, msg, Version.DRAFT_06), Version.DRAFT_06),
		).rejects.toThrow();
	}

	// Lite-07 claims name their route and carry no hop chain.
	const literal: AnnounceBroadcast = { status: "active", suffix: Path.from("x"), route: 2n, instance: a, epoch: 1n };
	expect(await roundTrip(literal, v)).toEqual({ ...literal, cost: 0n });
	await expect(bytes((w) => encodeAnnounceBroadcast(w, { ...literal, route: undefined }, v), v)).rejects.toThrow();
	await expect(bytes((w) => encodeAnnounceBroadcast(w, { ...literal, hops: [toHop(1n)] }, v), v)).rejects.toThrow();
});

test("the route layer refuses Node ID 0, unknown flags, and too many hops", async () => {
	const v = Version.DRAFT_07;
	const offer: Offer = {
		path: [{ node: 1n, session: 0n }],
		draining: false,
		priority: 0,
		metric: 0n,
		downOnly: false,
	};
	const encode = (msg: AnnounceBroadcast) => bytes((w) => encodeAnnounceBroadcast(w, msg, v), v);
	// No node sends an offer it would refuse.
	await expect(
		encode({ status: "routeStart", node: 0n, prefix: 0n, suffix: Path.empty(), exact: false, offer }),
	).rejects.toThrow();
	await expect(encode({ status: "routeUpdate", id: 0n, offer: { ...offer, path: [] } })).rejects.toThrow();
	await expect(
		encode({ status: "routeUpdate", id: 0n, offer: { ...offer, path: [{ node: 0n, session: 0n }] } }),
	).rejects.toThrow();
	const crowded = Array.from({ length: ROUTE_MAX_HOPS + 1 }, () => ({ node: 1n, session: 0n }));
	await expect(encode({ status: "routeUpdate", id: 0n, offer: { ...offer, path: crowded } })).rejects.toThrow();
	await expect(
		encode({
			status: "dead",
			dead: {
				kind: "closed",
				hop: { node: 0n, session: 0n },
				far: 2n,
				gone: false,
				moved: 0n,
			},
		}),
	).rejects.toThrow();

	// Built by hand, as only a nonconforming peer sends them.
	const forged = async (type: number, body: (w: Writer) => Promise<void>) => {
		const inner = await bytes(body, v);
		return bytes(async (w) => {
			await w.u53(type);
			await w.u53(inner.byteLength);
			await w.write(inner);
		}, v);
	};
	// A ROUTE_START to `node` along `path`, its flags `flags`, and nothing after them.
	const start = (node: bigint, path: bigint[], flags: number) =>
		forged(6, async (w) => {
			await w.u62(node);
			await w.u62(5n);
			await w.u53(0);
			// Not exact.
			await w.u8(0);
			await w.u62(BigInt(path.length));
			for (const hop of path) {
				await w.u62(hop);
				await w.u62(0n);
			}
			await w.u8(flags);
			await w.u8(128);
			await w.u62(0n);
		});
	// What the sender only forwarded is refused, the message decoded all the same, so the receiver
	// withdraws the route and keeps the stream, with the path a later DEAD may name.
	const decode = (wire: Uint8Array) => decodeAnnounceBroadcast(new Reader(undefined, wire, v), v);
	const path = [{ node: 1n, session: 0n }];
	expect(await decode(await start(0n, [1n], 0))).toEqual({
		status: "refused",
		message: { kind: "routeStart", node: 0n, prefix: 5n, suffix: Path.empty(), exact: false, path },
		reason: "node",
	});
	expect(await decode(await start(1n, [0n], 0))).toMatchObject({ status: "refused", reason: "node" });
	// A flag no version defines is refused.
	for (const flags of [4, 8]) {
		expect(await decode(await start(1n, [1n], flags))).toEqual({
			status: "refused",
			message: { kind: "routeStart", node: 1n, prefix: 5n, suffix: Path.empty(), exact: false, path },
			reason: "flags",
		});
	}
	// More hops than a route may carry.
	const over = Array.from({ length: ROUTE_MAX_HOPS + 1 }, () => 1n);
	expect(await decode(await start(1n, over, 0))).toMatchObject({ status: "refused", reason: "hops" });
	// Far past the limit, only the hops up to one past it are kept.
	const far = Array.from({ length: 4 * (ROUTE_MAX_HOPS + 1) }, () => 1n);
	const kept = await decode(await start(1n, far, 0));
	expect(kept).toMatchObject({ status: "refused", reason: "hops" });
	if (kept.status !== "refused" || kept.message.kind !== "routeStart") throw new Error("a refused ROUTE_START");
	expect(kept.message.path).toHaveLength(ROUTE_MAX_HOPS + 1);
	// An empty path is the sender's own error: it names itself last.
	await expect(decode(await start(1n, [], 0))).rejects.toThrow(ProtocolViolation);
	// Exact is a boolean.
	const notBoolean = await forged(6, async (w) => {
		await w.u62(1n);
		await w.u62(0n);
		await w.string("");
		await w.u8(2);
		await w.u62(1n);
		await w.u62(1n);
		await w.u62(0n);
		await w.u8(0);
		await w.u8(128);
		await w.u62(0n);
	});
	await expect(decode(notBoolean)).rejects.toThrow();

	// A DEAD counts the ROUTE_UPDATEs that follow, each refused on its own.
	const dead = await forged(15, async (w) => {
		await w.u62(0n);
		await w.u62(1n);
		await w.u62(0n);
		await w.u62(2n);
		await w.u8(0);
		await w.u62(2n);
	});
	expect(await decode(dead)).toEqual({
		status: "dead",
		dead: { kind: "closed", hop: { node: 1n, session: 0n }, far: 2n, gone: false, moved: 2n },
	});
	const update = (flags: number) =>
		forged(7, async (w) => {
			await w.u62(3n);
			await w.u62(1n);
			await w.u62(1n);
			await w.u62(0n);
			await w.u8(flags);
			await w.u8(128);
			await w.u62(0n);
		});
	expect(await decode(await update(8))).toEqual({
		status: "refused",
		message: { kind: "routeUpdate", id: 3n, path },
		reason: "flags",
	});
	expect(await decode(await update(0))).toEqual({
		status: "routeUpdate",
		id: 3n,
		offer: { path, draining: false, priority: 0, metric: 0n, downOnly: false },
	});
	// A Gone that is no boolean is malformed.
	const gone = await forged(15, async (w) => {
		await w.u62(0n);
		await w.u62(1n);
		await w.u62(0n);
		await w.u62(2n);
		await w.u8(2);
		await w.u62(0n);
	});
	await expect(decode(gone)).rejects.toThrow();
});

test("AnnounceHistory rejects a base that is not live", () => {
	const history = new AnnounceHistory();
	const base = { distance: 1n, keep: 0 };
	// A new stream has nothing to copy from.
	expect(() => history.start({ suffix: Path.from("a"), pathBase: base })).toThrow(ProtocolViolation);

	const fresh = new AnnounceHistory();
	fresh.start({ suffix: Path.from("a/b") });
	fresh.start({ suffix: Path.from("c") });
	fresh.end(0n);
	// Id 0 is two back, and retired.
	expect(() => fresh.start({ suffix: Path.from("x"), pathBase: { distance: 2n, keep: 1 } })).toThrow(
		ProtocolViolation,
	);
});

test("AnnounceHistory rejects a keep longer than the base", () => {
	const history = new AnnounceHistory();
	history.start({ suffix: Path.from("a/b") });
	expect(() => history.start({ suffix: Path.from("x"), pathBase: { distance: 1n, keep: 3 } })).toThrow(
		ProtocolViolation,
	);
});

test("AnnounceHistory keeps segments split at an escaped slash", () => {
	const history = new AnnounceHistory();
	history.start({ suffix: Path.from("a\\/b") });
	// The kept `a\` is half an escape, which the rest completes, or nothing does and the path is
	// refused, its id assigned all the same.
	const suffix = history.start({ suffix: Path.from("c"), pathBase: { distance: 1n, keep: 1 } });
	expect(suffix).toBe(Path.from("a\\/c"));
	expect(history.start({ suffix: Path.empty(), pathBase: { distance: 1n, keep: 1 } })).toBeUndefined();
	expect(history.next).toBe(3n);
});

test("AnnounceHistory refuses a resolved path past the byte limit", () => {
	const history = new AnnounceHistory();
	history.start({ suffix: Path.from("x".repeat(Path.MAX_BYTES - 2)) });
	// `head/y` is exactly the limit, and one byte more is refused as content: the id is assigned,
	// a base naming it is refused too, and its end is no violation.
	const suffix = history.start({ suffix: Path.from("y"), pathBase: { distance: 1n, keep: 1 } });
	expect(suffix?.length).toBe(Path.MAX_BYTES);
	expect(history.start({ suffix: Path.from("yz"), pathBase: { distance: 1n, keep: 1 } })).toBeUndefined();
	expect(history.start({ suffix: Path.from("z"), pathBase: { distance: 1n, keep: 1 } })).toBeUndefined();
	expect(history.end(2n)).toBeUndefined();
	expect(() => history.end(2n)).toThrow(ProtocolViolation);
});

test("a keep without a base is a violation on draft-07", async () => {
	// ANNOUNCE_START: type, length, path base 0, path keep 1, empty suffix, empty hops, cost.
	const wire = await bytes(async (w) => {
		await w.u53(0);
		await w.u53(8);
		for (const b of [0, 1, 0, 0, 0, 0, 0, 0]) await w.u8(b);
	}, Version.DRAFT_07);
	await expect(
		decodeAnnounceBroadcast(new Reader(undefined, wire, Version.DRAFT_07), Version.DRAFT_07),
	).rejects.toThrow(ProtocolViolation);
});

test("draft-06 has no room for a base", async () => {
	const msg: AnnounceBroadcast = {
		status: "active",
		suffix: Path.from("x"),
		hops: [],
		pathBase: { distance: 1n, keep: 1 },
	};
	await expect(bytes((w) => encodeAnnounceBroadcast(w, msg, Version.DRAFT_06), Version.DRAFT_06)).rejects.toThrow();
});

// Costs saturate at 2^62-1 on every version: a larger one from a lite-07 peer reads as the
// ceiling, and one set locally goes out as the ceiling, so a cost always forwards to lite-06.
test("route costs saturate at 2^62-1 on every version", async () => {
	const ceiling = 2n ** 62n - 1n;
	const huge = 2n ** 64n - 1n;
	for (const version of [Version.DRAFT_06, Version.DRAFT_07]) {
		const lite07 = version === Version.DRAFT_07;
		const msg: AnnounceBroadcast = lite07
			? { status: "active", suffix: Path.from("x"), route: 0n, instance: a, epoch: 0n, cost: huge }
			: { status: "active", suffix: Path.from("x"), hops: [], cost: huge };
		expect(await roundTrip(msg, version)).toMatchObject({ cost: ceiling });
	}

	// ANNOUNCE_START: path base, path keep, empty suffix, route 0, instance "a", unminted, epoch 0,
	// then a cost of 2^64-1, which only lite-07's varints can carry.
	const wire = await bytes(async (w) => {
		await w.u53(0);
		await w.u53(17);
		for (const byte of [0, 0, 0, 0, 1, 0x61, 0, 0]) await w.u8(byte);
		await w.u62(huge);
	}, Version.DRAFT_07);
	const got = await decodeAnnounceBroadcast(new Reader(undefined, wire, Version.DRAFT_07), Version.DRAFT_07);
	expect(got).toMatchObject({ cost: ceiling });
});

test("wip start and update carry one static cost", async () => {
	const v = Version.DRAFT_07;
	const start = await bytes(
		(w) =>
			encodeAnnounceBroadcast(
				w,
				{ status: "active", suffix: Path.empty(), route: 0n, instance: b, cost: Cost.zero },
				v,
			),
		v,
	);
	// Path base, path keep, empty suffix, route 0, instance "b", unminted, epoch 0, cost.
	expect([...start]).toEqual([0, 9, 0, 0, 0, 0, 1, 0x62, 0, 0, 0]);
	const update = await bytes((w) => encodeAnnounceBroadcast(w, { status: "update", id: 0n, cost: Cost.zero }, v), v);
	expect([...update]).toEqual([2, 2, 0, 0]);
});

test("lite06 ignores a legacy cold price and writes its saturation ceiling", async () => {
	const v = Version.DRAFT_06;
	for (const cold of [0n, 3n, 2n ** 62n - 1n]) {
		const wire = await bytes(async (w) => {
			await w.u53(0);
			const body = await bytes(async (b) => {
				await b.string("");
				await b.u53(0);
				await b.u62(12n);
				await b.u62(cold);
			}, v);
			await w.u53(body.length);
			await w.write(body);
		}, v);
		expect(await decodeAnnounceBroadcast(new Reader(undefined, wire, v), v)).toMatchObject({ cost: 12n });
	}
	const wire = await bytes(
		(w) => encodeAnnounceBroadcast(w, { status: "active", suffix: Path.empty(), hops: [], cost: 12n }, v),
		v,
	);
	const r = new Reader(undefined, wire, v);
	expect(await r.u53()).toBe(0);
	await r.u53();
	await r.string();
	await r.u53();
	expect(await r.u62()).toBe(12n);
	expect(await r.u62()).toBe(2n ** 62n - 1n);
});

test("SYNCED names its Prefix ID and belongs only to lite-07", async () => {
	const synced: AnnounceBroadcast = { status: "synced", prefix: 5n };
	expect(await bytes((w) => encodeAnnounceBroadcast(w, synced, Version.DRAFT_07), Version.DRAFT_07)).toEqual(
		new Uint8Array([4, 1, 5]),
	);
	expect(await roundTrip(synced, Version.DRAFT_07)).toEqual(synced);
	await expect(roundTrip(synced, Version.DRAFT_06)).rejects.toThrow();
});

// Lite-07 answers an edit with an empty message on its Announce Stream: SYNCED carries the
// count's role, and the route layer the Hop ID's.
test("ANNOUNCE_OK is an empty Announce Stream message on lite-07", async () => {
	const v = Version.DRAFT_07;
	const wire = await bytes((w) => new AnnounceOk(toHop(1n), 7).encode(w, Version.DRAFT_06), Version.DRAFT_06);
	expect(wire).toEqual(new Uint8Array([2, 1, 7]));
	await expect(bytes((w) => new AnnounceOk(toHop(1n), 7).encode(w, v), v)).rejects.toThrow();
	expect(await bytes((w) => encodeAnnounceBroadcast(w, { status: "ok" }, v), v)).toEqual(new Uint8Array([17, 0]));
	expect(await roundTrip({ status: "ok" }, v)).toEqual({ status: "ok" });
	await expect(roundTrip({ status: "ok" }, Version.DRAFT_06)).rejects.toThrow();
});

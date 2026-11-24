import { expect, test } from "bun:test";
import { ProtocolViolation } from "../error.ts";
import { toHop, UNKNOWN_HOP } from "../hop.ts";
import * as Instance from "../instance.ts";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import {
	type AnnounceBroadcast,
	AnnounceHistory,
	AnnounceOk,
	AnnounceRequest,
	CHAIN_MAX,
	type Claim,
	decodeAnnounceBroadcast,
	decodeAnnounceBroadcastMaybe,
	encodeAnnounceBroadcast,
	ownClaim,
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

test("AnnounceBroadcast skips an unknown type on draft-06", async () => {
	const encoded = await bytes(async (w) => {
		await w.u53(4);
		await w.u53(1);
		await w.u8(0);
	}, Version.DRAFT_06);
	const reader = new Reader(undefined, encoded, Version.DRAFT_06);
	expect(await decodeAnnounceBroadcast(reader, Version.DRAFT_06)).toEqual({ status: "skipped" });
});

test("AnnounceBroadcast rejects an unknown type on draft-07", async () => {
	const encoded = await bytes(async (w) => {
		await w.u53(11);
		await w.u53(2);
		await w.u8(0);
		await w.u8(0);
	}, Version.DRAFT_07);
	const reader = new Reader(undefined, encoded, Version.DRAFT_07);
	await expect(decodeAnnounceBroadcast(reader, Version.DRAFT_07)).rejects.toThrow(ProtocolViolation);
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
	expect(wire).toEqual(new Uint8Array([5, 13, 1, 2, 1, 2, 2, 4, 0x72, 0x6f, 0x6f, 0x6d, 1, 0, 0]));
	expect(await roundTrip(msg, v)).toEqual(msg);
	const empty: AnnounceBroadcast = { status: "request", more: false, remove: [], add: [] };
	expect(await bytes((w) => encodeAnnounceBroadcast(w, empty, v), v)).toEqual(new Uint8Array([5, 3, 0, 0, 0]));
	await expect(roundTrip(msg, Version.DRAFT_06)).rejects.toThrow();
	await expect(bytes((w) => new AnnounceRequest(Path.from("room"), UNKNOWN_HOP).encode(w, v), v)).rejects.toThrow();

	// A count the body cannot hold, and a More or a Hidden that is no boolean, are malformed.
	const decode = (body: number[]) =>
		decodeAnnounceBroadcast(new Reader(undefined, new Uint8Array([5, body.length, ...body]), v), v);
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

// Resolve every claim on a lite-07 stream, as the subscriber does.
async function resolveStream(data: Uint8Array) {
	const reader = new Reader(undefined, data, Version.DRAFT_07);
	const history = new AnnounceHistory();
	const out: unknown[] = [];
	for (;;) {
		const msg = await decodeAnnounceBroadcastMaybe(reader, Version.DRAFT_07);
		if (!msg) return out;
		switch (msg.status) {
			case "start":
				out.push({ start: history.start(msg), instance: msg.instance, claim: msg.claim });
				break;
			case "change":
				history.get(msg.id);
				out.push({ update: msg.id, claim: msg.claim });
				break;
			case "endedId":
				out.push({ end: msg.id, suffix: history.end(msg.id), lost: msg.lost });
				break;
			default:
				throw new Error(`unexpected ${msg.status}`);
		}
	}
}

const a = Instance.parse("a");
const b = Instance.parse("b");
const own = { instance: a, claim: ownClaim(0n) };
const GOLDEN_RESOLVED = [
	{ start: Path.from("room/a/cam"), ...own },
	{ start: Path.from("room/a/mic"), ...own },
	{ update: 0n, claim: ownClaim(0n) },
	{ end: 1n, suffix: Path.from("room/a/mic"), lost: false },
	{ start: Path.from("room/b"), ...own },
];

// Rust's compressed output, pinned by `lite::compress::tests::golden_stream_is_pinned`.
const GOLDEN =
	"001600000a726f6f6d2f612f63616d01610000000000ff00000f0102036d696301610000000000ff0002070000000000ff00" +
	"01020100000d0201016201610000000000ff00";

test("AnnounceHistory resolves the Rust encoder's compressed stream", async () => {
	expect(await resolveStream(unhex(GOLDEN))).toEqual(GOLDEN_RESOLVED);
});

// Rust's compressed output for bases that keep parts holding `\/` and `\\`, pinned by
// `lite::compress::tests::golden_escaped_stream_is_pinned`.
const GOLDEN_ESCAPED =
	"001b00000f726f6f6d2f615c2f622f63616d2d3101610000000000ff00001701010b635c5c2f642f63616d2d3101610000000000ff00001102020563616d2d3201610000000000ff00000d0202016501610000000000ff00";

test("AnnounceHistory resolves the Rust encoder's keeps across escapes", async () => {
	expect(await resolveStream(unhex(GOLDEN_ESCAPED))).toEqual([
		{ start: Path.from("room/a\\/b/cam-1"), ...own },
		{ start: Path.from("room/c\\\\/d/cam-1"), ...own },
		{ start: Path.from("room/a\\/b/cam-2"), ...own },
		{ start: Path.from("room/c\\\\/e"), ...own },
	]);
});

// JS always encodes literally. Rust's `js_literal_stream_decodes` pins the same bytes.
const JS_LITERAL =
	"001600000a726f6f6d2f612f63616d01610000000000ff00001600000a726f6f6d2f612f6d696301610000000000ff00" +
	"02070000000000ff00010201000012000006726f6f6d2f6201610000000000ff00";

test("the literal draft-07 stream matches what Rust decodes", async () => {
	const wire = await bytes(async (w) => {
		const v = Version.DRAFT_07;
		const start = (suffix: string): AnnounceBroadcast => ({ status: "start", suffix: Path.from(suffix), ...own });
		await encodeAnnounceBroadcast(w, start("room/a/cam"), v);
		await encodeAnnounceBroadcast(w, start("room/a/mic"), v);
		await encodeAnnounceBroadcast(w, { status: "change", id: 0n, claim: ownClaim(0n) }, v);
		await encodeAnnounceBroadcast(w, { status: "endedId", id: 1n, lost: false }, v);
		await encodeAnnounceBroadcast(w, start("room/b"), v);
	}, Version.DRAFT_07);
	expect(hex(wire)).toBe(JS_LITERAL);
	expect(await resolveStream(wire)).toEqual(GOLDEN_RESOLVED);
});

// Rust's `a_leading_bom_is_part_of_the_path` pins the same bytes: U+FEFF opens the path's text.
const BOM_LITE06 = "00150aefbbbf636f6e636572740000ffffffffffffffff";

test("a path opening with U+FEFF keeps it across the lite-06 wire", async () => {
	const v = Version.DRAFT_06;
	const msg: AnnounceBroadcast = { status: "active", suffix: Path.from("\uFEFFconcert"), hops: [] };
	expect(hex(await bytes((w) => encodeAnnounceBroadcast(w, msg, v), v))).toBe(BOM_LITE06);
	expect(await decodeAnnounceBroadcast(new Reader(undefined, unhex(BOM_LITE06), v), v)).toEqual({ ...msg, cost: 0n });
});

// A claim says its cost, how far above its path it is, whether it is unavailable, draining, or
// behind a marked session, the lowest priority on its way, and the relays it entered or left a
// routing domain by.
test("a lite-07 claim round-trips with its exact bytes", async () => {
	const v = Version.DRAFT_07;
	const claim: Claim = {
		epoch: 7n,
		cost: 12n,
		above: 2n,
		unavailable: true,
		draining: true,
		marked: true,
		priority: -3,
		chain: [5n, 9n],
	};
	const cases: [AnnounceBroadcast, number[]][] = [
		// Path base, path keep, the suffix, instance "b", unminted, epoch, then the claim: cost,
		// above, all three flags, the priority with its sign bit flipped, and the chain.
		[
			{ status: "start", suffix: Path.from("x"), instance: b, claim },
			[0, 15, 0, 0, 1, 0x78, 1, 0x62, 0, 7, 12, 2, 7, 0x7d, 2, 5, 9],
		],
		[{ status: "change", id: 1n, claim }, [2, 9, 1, 7, 12, 2, 7, 0x7d, 2, 5, 9]],
		[{ status: "endedId", id: 1n, lost: true }, [1, 2, 1, 1]],
		[{ status: "endedId", id: 1n, lost: false }, [1, 2, 1, 0]],
	];
	for (const [msg, wire] of cases) {
		expect([...(await bytes((w) => encodeAnnounceBroadcast(w, msg, v), v))]).toEqual(wire);
		expect(await roundTrip(msg, v)).toEqual(msg);
	}
	// An own claim is the bytes every endpoint sends: no cost, the top priority, no chain.
	const start = await bytes(
		(w) =>
			encodeAnnounceBroadcast(w, { status: "start", suffix: Path.empty(), instance: b, claim: ownClaim(0n) }, v),
		v,
	);
	expect([...start]).toEqual([0, 12, 0, 0, 0, 1, 0x62, 0, 0, 0, 0, 0, 0xff, 0]);

	// Older versions have no claims, and lite-07 has nothing else.
	const start06: AnnounceBroadcast = { status: "start", suffix: Path.from("x"), instance: b, claim };
	await expect(
		bytes((w) => encodeAnnounceBroadcast(w, start06, Version.DRAFT_06), Version.DRAFT_06),
	).rejects.toThrow();
	await expect(
		bytes((w) => encodeAnnounceBroadcast(w, { status: "active", suffix: Path.from("x"), hops: [] }, v), v),
	).rejects.toThrow();
	await expect(
		bytes((w) => encodeAnnounceBroadcast(w, { status: "update", id: 0n, hops: [] }, v), v),
	).rejects.toThrow();
});

test("a lite-07 claim refuses Node ID 0, unknown flags, and too long a chain", async () => {
	const v = Version.DRAFT_07;
	const change = (claim: Claim) =>
		bytes((w) => encodeAnnounceBroadcast(w, { status: "change", id: 0n, claim }, v), v);
	const full = Array.from({ length: CHAIN_MAX }, (_, i) => BigInt(i + 1));
	expect(await roundTrip({ status: "change", id: 0n, claim: { ...ownClaim(0n), chain: full } }, v)).toMatchObject({
		claim: { chain: full },
	});
	await expect(change({ ...ownClaim(0n), chain: [0n] })).rejects.toThrow();
	await expect(change({ ...ownClaim(0n), chain: [...full, 99n] })).rejects.toThrow();

	// ANNOUNCE_UPDATE: id, epoch, cost, above, flags, priority, then the chain.
	const decode = (...claim: number[]) => {
		const body = [0, 0, 0, 0, ...claim];
		return decodeAnnounceBroadcast(new Reader(undefined, new Uint8Array([2, body.length, ...body]), v), v);
	};
	expect(await decode(0, 0xff, 0)).toEqual({ status: "change", id: 0n, claim: ownClaim(0n) });
	await expect(decode(8, 0xff, 0)).rejects.toThrow(ProtocolViolation);
	await expect(decode(0, 0xff, 1, 0)).rejects.toThrow(ProtocolViolation);
	await expect(decode(0, 0xff, CHAIN_MAX + 1, ...full.map(Number), 40)).rejects.toThrow(ProtocolViolation);
});

// Only two nodes that both declared a Node ID exchange routes, and an endpoint declares none.
test("a route message to an endpoint is a violation on lite-07", async () => {
	for (const typ of [3, 7, 8, 9, 10]) {
		const wire = new Uint8Array([typ, 1, 0]);
		await expect(
			decodeAnnounceBroadcast(new Reader(undefined, wire, Version.DRAFT_07), Version.DRAFT_07),
		).rejects.toThrow(ProtocolViolation);
		// Earlier versions never defined them, so they skip them as unknown.
		expect(await decodeAnnounceBroadcast(new Reader(undefined, wire, Version.DRAFT_06), Version.DRAFT_06)).toEqual({
			status: "skipped",
		});
	}
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

// A keep counts decoded parts: an escaped `/` stays inside its part, and a run of `\\` before a
// `/` leaves that `/` bare. rs/moq-net's compress.rs pins the same cases.
test("AnnounceHistory keeps whole parts across an escape", () => {
	const history = new AnnounceHistory();
	history.start({ suffix: Path.from("room/a\\/b/cam-1") });
	history.start({ suffix: Path.from("room/c\\\\/d/cam-1") });
	// `room` and `a\/b` are two parts.
	expect(history.start({ suffix: Path.from("cam-2"), pathBase: { distance: 2n, keep: 2 } })).toBe(
		Path.from("room/a\\/b/cam-2"),
	);
	// `c\\` ends in an escaped backslash, so its `/` separates.
	expect(history.start({ suffix: Path.from("e"), pathBase: { distance: 2n, keep: 2 } })).toBe(
		Path.from("room/c\\\\/e"),
	);
	// Three parts of `room/a\/b/cam-1` are all it has, and a fourth is refused.
	expect(history.start({ suffix: Path.from("x"), pathBase: { distance: 4n, keep: 3 } })).toBe(
		Path.from("room/a\\/b/cam-1/x"),
	);
	expect(() => history.start({ suffix: Path.empty(), pathBase: { distance: 5n, keep: 4 } })).toThrow(
		ProtocolViolation,
	);
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

// Costs saturate at 2^62-1 on every version: a larger one from a lite-07 peer reads as the
// ceiling, and one set locally goes out as the ceiling, so a cost always forwards to lite-06.
test("route costs saturate at 2^62-1 on every version", async () => {
	const ceiling = 2n ** 62n - 1n;
	const huge = 2n ** 64n - 1n;
	const claimed: AnnounceBroadcast = {
		status: "start",
		suffix: Path.from("x"),
		instance: a,
		claim: { ...ownClaim(0n), cost: huge },
	};
	expect(await roundTrip(claimed, Version.DRAFT_07)).toMatchObject({ claim: { cost: ceiling } });
	const active: AnnounceBroadcast = { status: "active", suffix: Path.from("x"), hops: [], cost: huge };
	expect(await roundTrip(active, Version.DRAFT_06)).toMatchObject({ cost: ceiling });

	// ANNOUNCE_START: path base, path keep, empty suffix, instance "a", unminted, epoch 0, then a
	// cost of 2^64-1, which only lite-07's varints can carry, and the rest of an own claim.
	const wire = await bytes(async (w) => {
		await w.u53(0);
		await w.u53(20);
		for (const byte of [0, 0, 0, 1, 0x61, 0, 0]) await w.u8(byte);
		await w.u62(huge);
		for (const byte of [0, 0, 0xff, 0]) await w.u8(byte);
	}, Version.DRAFT_07);
	const got = await decodeAnnounceBroadcast(new Reader(undefined, wire, Version.DRAFT_07), Version.DRAFT_07);
	expect(got).toMatchObject({ claim: { cost: ceiling } });
});

test("a claim above its path travels only on lite-07", async () => {
	const v = Version.DRAFT_07;
	const start: AnnounceBroadcast = { status: "start", suffix: Path.empty(), instance: b, claim: ownClaim(1n, 3n) };
	expect(await roundTrip(start, v)).toEqual(start);
	const update: AnnounceBroadcast = { status: "change", id: 0n, claim: ownClaim(1n, 2n) };
	expect(await roundTrip(update, v)).toEqual(update);
	// Id, epoch, cost, above, flags, priority, chain.
	expect([...(await bytes((w) => encodeAnnounceBroadcast(w, update, v), v))]).toEqual([2, 7, 0, 1, 0, 2, 0, 0xff, 0]);
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
// count's role, and nothing the Hop ID's.
test("ANNOUNCE_OK is an empty Announce Stream message on lite-07", async () => {
	const v = Version.DRAFT_07;
	const wire = await bytes((w) => new AnnounceOk(toHop(1n), 7).encode(w, Version.DRAFT_06), Version.DRAFT_06);
	expect(wire).toEqual(new Uint8Array([2, 1, 7]));
	await expect(bytes((w) => new AnnounceOk(toHop(1n), 7).encode(w, v), v)).rejects.toThrow();
	expect(await bytes((w) => encodeAnnounceBroadcast(w, { status: "ok" }, v), v)).toEqual(new Uint8Array([6, 0]));
	expect(await roundTrip({ status: "ok" }, v)).toEqual({ status: "ok" });
	await expect(roundTrip({ status: "ok" }, Version.DRAFT_06)).rejects.toThrow();
});

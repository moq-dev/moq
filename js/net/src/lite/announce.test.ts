import { expect, test } from "bun:test";
import { ProtocolViolation } from "../error.ts";
import { Cost, HopSchema, UNKNOWN_HOP } from "../hop.ts";
import * as Instance from "../instance.ts";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import {
	type AnnounceBroadcast,
	AnnounceHistory,
	AnnounceOk,
	type AnnounceReply,
	AnnounceRequest,
	decodeAnnounceBroadcast,
	decodeAnnounceBroadcastMaybe,
	decodeAnnounceReplyMaybe,
	encodeAnnounceBroadcast,
	encodeAnnounceReply,
	ROUTE_MAX,
	ROUTE_MAX_HOPS,
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
	const got = await requestRoundTrip(new AnnounceRequest(Path.from("room"), HopSchema.parse(42n)), Version.DRAFT_05);
	expect(got.excludeHop).toBe(HopSchema.parse(42n));
});

// Draft06 dropped the field: the receiver's reflected-announce check catches the same
// loops, so a value set locally is simply not sent and decodes as zero.
test("AnnounceRequest drops excludeHop on draft-06", async () => {
	const msg = new AnnounceRequest(Path.from("room"), HopSchema.parse(42n));
	const got = await requestRoundTrip(msg, Version.DRAFT_06);
	expect(got.excludeHop).toBe(UNKNOWN_HOP);

	const with05 = await bytes((w) => msg.encode(w, Version.DRAFT_05), Version.DRAFT_05);
	const with06 = await bytes((w) => msg.encode(w, Version.DRAFT_06), Version.DRAFT_06);
	expect(with06.byteLength).toBeLessThan(with05.byteLength);
});

// Draft07 carries the hidden opt-in; every earlier version decodes as not opted in.
test("AnnounceRequest carries hidden from draft-07", async () => {
	for (const hidden of [false, true]) {
		const msg = new AnnounceRequest(Path.from("room"), UNKNOWN_HOP, hidden);
		expect((await requestRoundTrip(msg, Version.DRAFT_07)).hidden).toBe(hidden);
		expect((await requestRoundTrip(msg, Version.DRAFT_06)).hidden).toBe(false);
	}
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
	const msg = new AnnounceOk(HopSchema.parse(42n), 1);
	const reader = new Reader(
		undefined,
		await bytes((w) => msg.encode(w, Version.DRAFT_05), Version.DRAFT_05),
		Version.DRAFT_05,
	);
	const got = await AnnounceOk.decode(reader, Version.DRAFT_05);
	expect(got.hop).toBe(HopSchema.parse(42n));
	expect(got.active).toBe(1);
});

// A QUIC varint stops at 2^62-1, so draft-06 carries a wider hop as unknown.
test("a 64-bit hop is unknown on draft-06", async () => {
	const version = Version.DRAFT_06;
	const wide = HopSchema.parse((1n << 64n) - 1n);
	const got = await roundTrip({ status: "active", suffix: Path.from("room"), hops: [wide] }, version);
	expect(got).toMatchObject({ hops: [UNKNOWN_HOP] });

	const msg = new AnnounceOk(wide, 0);
	const reader = new Reader(undefined, await bytes((w) => msg.encode(w, version), version), version);
	expect((await AnnounceOk.decode(reader, version)).hop).toBe(UNKNOWN_HOP);
});

test("a hop chain that revisits a hop is refused in both directions", async () => {
	const four = HopSchema.parse(4n);
	const eight = HopSchema.parse(8n);
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
		hops: [four, eight, HopSchema.parse(9n)],
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
	const advert = { seqno: 3n, metric: 5n, downOnly: true, rtt: undefined, hops: [7n, 9n] };
	const cases: [AnnounceBroadcast, number[]][] = [
		[{ status: "routeStart", node: 0x1234n, advert }, [6, 8, 0x92, 0x34, 3, 5, 1, 2, 7, 9]],
		[{ status: "routeUpdate", id: 1n, advert: { ...advert, hops: [] } }, [7, 5, 1, 3, 5, 1, 0]],
		// Flag 0x2: an RTT follows, 0 at the origin, whose Hops are its own alone.
		[
			{ status: "routeStart", node: 0x1234n, advert: { ...advert, downOnly: false, rtt: 0n, hops: [0x1234n] } },
			[6, 9, 0x92, 0x34, 3, 5, 2, 0, 1, 0x92, 0x34],
		],
		[{ status: "routeUpdate", id: 1n, advert: { ...advert, rtt: 25n, hops: [] } }, [7, 6, 1, 3, 5, 3, 25, 0]],
		[
			{ status: "routeUpdate", id: 1n, advert: { ...advert, rtt: ROUTE_MAX, hops: [ROUTE_MAX] } },
			[7, 23, 1, 3, 5, 3, ...Array(9).fill(0xff), 1, ...Array(9).fill(0xff)],
		],
		// As many hops as a route may carry.
		[
			{
				status: "routeUpdate",
				id: 1n,
				advert: { ...advert, hops: Array.from({ length: ROUTE_MAX_HOPS }, (_, i) => BigInt(i + 1)) },
			},
			[7, 37, 1, 3, 5, 1, ROUTE_MAX_HOPS, ...Array.from({ length: ROUTE_MAX_HOPS }, (_, i) => i + 1)],
		],
		[{ status: "routeHold", id: 1n }, [0xb, 1, 1]],
		[{ status: "routeEnd", id: 1n }, [8, 1, 1]],
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
	await expect(
		bytes((w) => encodeAnnounceBroadcast(w, { ...literal, hops: [HopSchema.parse(1n)] }, v), v),
	).rejects.toThrow();
});

test("the route layer refuses the last seqno and Node ID 0", async () => {
	const v = Version.DRAFT_07;
	const last = { seqno: ROUTE_MAX, metric: 0n, downOnly: false, rtt: undefined, hops: [] };
	await expect(
		bytes((w) => encodeAnnounceBroadcast(w, { status: "routeStart", node: 1n, advert: last }, v), v),
	).rejects.toThrow();
	const crowded = { ...last, seqno: 0n, hops: Array.from({ length: ROUTE_MAX_HOPS + 1 }, () => 1n) };
	await expect(
		bytes((w) => encodeAnnounceBroadcast(w, { status: "routeStart", node: 1n, advert: crowded }, v), v),
	).rejects.toThrow();
	await expect(
		bytes((w) => encodeAnnounceReply(w, { kind: "request", node: 0n, seqno: 1n }, v), v),
	).rejects.toThrow();

	// Built by hand, as only a nonconforming peer sends them.
	const forged = async (body: (w: Writer) => Promise<void>) => {
		const inner = await bytes(body, v);
		return bytes(async (w) => {
			await w.u53(6);
			await w.u53(inner.byteLength);
			await w.write(inner);
		}, v);
	};
	const zero = await forged(async (w) => {
		await w.u62(0n);
		await w.u62(0n);
		await w.u62(0n);
		await w.u8(0);
	});
	await expect(decodeAnnounceBroadcast(new Reader(undefined, zero, v), v)).rejects.toThrow(ProtocolViolation);
	const maxed = await forged(async (w) => {
		await w.u62(1n);
		await w.u62(ROUTE_MAX);
		await w.u62(0n);
		await w.u8(0);
	});
	await expect(decodeAnnounceBroadcast(new Reader(undefined, maxed, v), v)).rejects.toThrow(ProtocolViolation);
	// A flag no version defines, and the RTT flag without its field.
	for (const flags of [4, 2]) {
		const flagged = await forged(async (w) => {
			await w.u62(1n);
			await w.u62(0n);
			await w.u62(0n);
			await w.u8(flags);
		});
		await expect(decodeAnnounceBroadcast(new Reader(undefined, flagged, v), v)).rejects.toThrow();
	}
	// More hops than a route may carry, refused before reading them.
	const overHops = await forged(async (w) => {
		await w.u62(1n);
		await w.u62(0n);
		await w.u62(0n);
		await w.u8(0);
		await w.u53(ROUTE_MAX_HOPS + 1);
	});
	await expect(decodeAnnounceBroadcast(new Reader(undefined, overHops, v), v)).rejects.toThrow(ProtocolViolation);
});

test("the subscriber's replies round-trip on draft-07", async () => {
	const v = Version.DRAFT_07;
	for (const [msg, wire] of [
		[{ kind: "request", node: 7n, seqno: 2n }, [0xa, 2, 7, 2]],
		[{ kind: "echo", id: 3n }, [8, 1, 3]],
	] as [AnnounceReply, number[]][]) {
		const encoded = await bytes((w) => encodeAnnounceReply(w, msg, v), v);
		expect([...encoded]).toEqual(wire);
		expect(await decodeAnnounceReplyMaybe(new Reader(undefined, encoded, v), v)).toEqual(msg);
	}
	// Anything else is a violation.
	const other = await bytes(async (w) => {
		await w.u53(0);
		await w.u53(0);
	}, v);
	await expect(decodeAnnounceReplyMaybe(new Reader(undefined, other, v), v)).rejects.toThrow(ProtocolViolation);
	// A finished stream is no reply.
	expect(await decodeAnnounceReplyMaybe(new Reader(undefined, new Uint8Array(), v), v)).toBeUndefined();
	await expect(
		bytes((w) => encodeAnnounceReply(w, { kind: "echo", id: 1n }, Version.DRAFT_06), Version.DRAFT_06),
	).rejects.toThrow();
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

test("SYNCED is empty and belongs only to lite-07", async () => {
	expect(
		await bytes((w) => encodeAnnounceBroadcast(w, { status: "synced" }, Version.DRAFT_07), Version.DRAFT_07),
	).toEqual(new Uint8Array([4, 0]));
	expect(await roundTrip({ status: "synced" }, Version.DRAFT_07)).toEqual({ status: "synced" });
	await expect(roundTrip({ status: "synced" }, Version.DRAFT_06)).rejects.toThrow();
});

// Lite-07 drops both: the count for SYNCED, and the Hop ID for the route layer.
test("ANNOUNCE_OK is empty on lite-07", async () => {
	for (const version of [Version.DRAFT_06, Version.DRAFT_07]) {
		const wire = await bytes((w) => new AnnounceOk(HopSchema.parse(1n), 7).encode(w, version), version);
		expect(wire).toEqual(new Uint8Array(version === Version.DRAFT_06 ? [2, 1, 7] : [0]));
	}
});

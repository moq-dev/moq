import { expect, test } from "bun:test";
import { ProtocolViolation, StreamCode, StreamError } from "../error.ts";
import { type Hop, HopSchema, UNKNOWN_HOP } from "../hop.ts";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import * as Cluster from "./cluster.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { Parameters } from "./parameters.ts";
import { PublishNamespace, PublishNamespaceUpdate } from "./publish_namespace.ts";
import { SetupOption, SetupOptions } from "./setup_options.ts";
import { SubscribeNamespaceEntry } from "./subscribe_namespace.ts";
import { MalformedAuthToken } from "./token.ts";
import { type IetfVersion, Version } from "./version.ts";

const VERSION = Version.DRAFT_19;

/** A session that negotiated MoQ Cluster alone. */
const CLUSTER = { cluster: true, epoch: false };

function origin(id: bigint): Hop {
	return HopSchema.parse(id);
}

function hops(...ids: bigint[]): Hop[] {
	return ids.map(origin);
}

async function encode<T extends { encode(w: Writer, v: IetfVersion): Promise<void> }>(
	message: T,
	version: IetfVersion = VERSION,
): Promise<Uint8Array> {
	const chunks: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({
			write(chunk) {
				chunks.push(new Uint8Array(chunk));
			},
		}),
		version,
	);
	await message.encode(writer, version);
	writer.close();
	await writer.closed;

	const out = new Uint8Array(chunks.reduce((total, chunk) => total + chunk.byteLength, 0));
	let offset = 0;
	for (const chunk of chunks) {
		out.set(chunk, offset);
		offset += chunk.byteLength;
	}
	return out;
}

function intoParams(advert: Cluster.Advert): Parameters {
	const params = new Parameters();
	Cluster.intoParams(advert, params);
	return params;
}

function reader(bytes: Uint8Array, version: IetfVersion = VERSION): Reader {
	return new Reader(undefined, bytes, version);
}

test("Cluster: the hop path carries no count of its own", async () => {
	// The entries fill the parameter length; a count would make us unreadable to every
	// other implementation.
	const params = intoParams({ hops: hops(1n, 2n, 3n), cost: 0n });
	expect(Array.from(await encode(params))).toEqual([
		0x01, // parameter count
		0xc4,
		0x0b,
		0x57, // HOP_PATH (0x40b57), delta encoded from 0
		0x03, // value length
		0x01,
		0x02,
		0x03, // the hop ids, filling the value
	]);
});

test("Cluster: an advertisement round trips through the parameters", async () => {
	const advert = { hops: hops(7n, 9n), cost: 5n };
	const encoded = await encode(intoParams(advert));
	const decoded = await Parameters.decode(reader(encoded), VERSION);
	expect(Cluster.fromParams(decoded)).toEqual(advert);
});

test("Cluster: a zero route cost is absent from the wire", async () => {
	const free = await encode(intoParams({ hops: hops(7n), cost: 0n }));
	const priced = await encode(intoParams({ hops: hops(7n), cost: 1n }));
	expect(free.length).toBeLessThan(priced.length);

	// Absent means 0, so it comes back as one.
	const decoded = await Parameters.decode(reader(free), VERSION);
	expect(Cluster.fromParams(decoded).cost).toBe(0n);
});

test("Cluster: a repeated zero is not a loop", async () => {
	const advert = { hops: hops(0n, 5n, 0n), cost: 0n };
	const encoded = await encode(intoParams(advert));
	const decoded = await Parameters.decode(reader(encoded), VERSION);
	expect(Cluster.fromParams(decoded)).toEqual(advert);
});

test("Cluster: a malformed advertisement is a session-fatal violation", () => {
	// The draft says a receiver MUST close the session over each of these, so they arrive
	// as one error the session dispatch can act on, not a stream-local decode failure.
	const missing = new Parameters();
	const empty = new Parameters();
	empty.hopPath = new Uint8Array();

	for (const params of [missing, empty]) {
		expect(() => Cluster.fromParams(params)).toThrow(ProtocolViolation);
	}
});

test("Cluster: a path that cannot have come from a conforming sender is rejected", () => {
	// A non-zero id appearing twice is a loop.
	expect(() => intoParams({ hops: hops(4n, 8n, 4n), cost: 0n })).toThrow();

	// The list always has at least one entry: the original publisher.
	const empty = new Parameters();
	empty.hopPath = new Uint8Array();
	expect(() => Cluster.fromParams(empty)).toThrow();

	// A negotiated session that omits HOP_PATH entirely is a protocol violation.
	expect(() => Cluster.fromParams(new Parameters())).toThrow();

	// Entries must exactly fill the length: 300 takes two bytes, so one byte ends
	// mid-varint.
	const partial = new Parameters();
	partial.hopPath = new Uint8Array([0x81]);
	expect(() => Cluster.fromParams(partial)).toThrow();

	// Longer than we accept, which a real path never is and a loop is.
	const long = Array.from({ length: 33 }, (_, i) => origin(BigInt(i + 1)));
	expect(() => intoParams({ hops: long, cost: 0n })).toThrow();
});

test("Cluster: the setup options round trip", async () => {
	for (const declared of [origin(42n), origin((1n << 64n) - 1n), UNKNOWN_HOP]) {
		const params = new SetupOptions();
		Cluster.intoSetup(params, declared, VERSION);

		const encoded = await encode(params);
		const decoded = await SetupOptions.decode(reader(encoded), VERSION);
		expect(Cluster.fromSetup(decoded, VERSION)).toBe(declared);
	}

	// A peer that declared nothing did not negotiate.
	expect(Cluster.fromSetup(new SetupOptions(), VERSION)).toBeUndefined();
});

test("Cluster: HOP_ID is a bare varint", async () => {
	// The key sits at delta 0x40b54 from the start of the block and the value follows it
	// directly: a length byte here would make us unreadable to every -01 peer.
	const params = new SetupOptions();
	Cluster.intoSetup(params, origin(42n), VERSION);
	expect(Array.from(await encode(params))).toEqual([
		0xc4,
		0x0b,
		0x54, // HOP_ID (0x40b54), delta encoded from 0
		0x2a, // the hop id, with no length prefix
	]);
});

test("Cluster: a peer declaring only RELAY_HOPS is not negotiated", async () => {
	// A -00 peer declares its Hop ID under the odd key 0x40b55 as a length-prefixed
	// varint. That key is unknown now, so the session runs as plain moq-transport rather
	// than misreading the value.
	const params = new SetupOptions();
	params.setBytes(0x40b55n, new Uint8Array([0x2a]));

	const decoded = await SetupOptions.decode(reader(await encode(params)), VERSION);
	expect(Cluster.fromSetup(decoded, VERSION)).toBeUndefined();
});

test("Cluster: the setup options are skipped before draft-17", () => {
	// Draft-14..16 exchange SETUP over the legacy control stream, which this extension
	// does not cover; we neither send nor read the options there.
	for (const version of [Version.DRAFT_14, Version.DRAFT_15, Version.DRAFT_16]) {
		expect(Cluster.supported(version)).toBe(false);

		const params = new SetupOptions();
		Cluster.intoSetup(params, origin(42n), version);
		expect(params.getVarint(SetupOption.HopId)).toBeUndefined();

		// Even one that arrived anyway reads as "not negotiated" there.
		Cluster.intoSetup(params, origin(42n), Version.DRAFT_19);
		expect(Cluster.fromSetup(params, version)).toBeUndefined();
	}
});

test("Cluster: we advertise our own id, and only once negotiated", () => {
	const self = origin(3n);

	// A peer that declared nothing gets no parameters at all.
	expect(Cluster.advertise(undefined)).toBeUndefined();
	expect(Cluster.advertise({ self })).toBeUndefined();
	expect(Cluster.negotiated({ self })).toBe(false);

	// A peer that declared the reserved 0 negotiated, and excludes nothing.
	expect(Cluster.negotiated({ self, peer: UNKNOWN_HOP })).toBe(true);
	expect(Cluster.advertise({ self, peer: origin(9n) })).toEqual({ hops: [self], cost: 0n });
});

test("Cluster: a loop is detected by any non-zero id", () => {
	const advert = { hops: hops(0n, 5n), cost: 0n };

	// A receiver whose own id is 0 cannot detect loops through itself.
	expect(Cluster.loops(advert, UNKNOWN_HOP)).toBe(false);
	expect(Cluster.loops(advert, origin(5n))).toBe(true);
	expect(Cluster.loops(advert, origin(6n))).toBe(false);
});

test("Cluster: PUBLISH_NAMESPACE carries the parameters only once negotiated", async () => {
	const advert = { hops: hops(7n, 9n), cost: 5n };
	const msg = new PublishNamespace({
		requestId: 1n,
		trackNamespace: Path.from("alice.hang"),
		params: { cluster: advert },
	});
	const encoded = await encode(msg);

	const decoded = await PublishNamespace.decode(reader(encoded), VERSION, CLUSTER);
	expect(decoded.trackNamespace).toBe(Path.from("alice.hang"));
	expect(decoded.params.cluster).toEqual(advert);

	// The message always has a Parameters field, so a session that negotiated nothing
	// reads the same bytes and ignores what it did not ask for.
	expect((await PublishNamespace.decode(reader(encoded), VERSION)).params.cluster).toBeUndefined();

	// A negotiated session that omits HOP_PATH is a protocol violation, which the session
	// dispatch closes over rather than losing only the stream.
	const bare = new PublishNamespace({ requestId: 1n, trackNamespace: Path.from("alice.hang") });
	await expect(PublishNamespace.decode(reader(await encode(bare)), VERSION, CLUSTER)).rejects.toThrow(
		ProtocolViolation,
	);
});

test("Cluster: a negotiated PUBLISH_NAMESPACE may carry AUTHORIZATION_TOKEN", async () => {
	// The advertisement shares its decode with NAMESPACE, whose list has no token. A
	// PUBLISH_NAMESPACE's own list does, so the token must not close the session.
	const advert = { hops: hops(7n, 9n), cost: 5n };
	const withToken = {
		async encode(w: Writer, version: IetfVersion) {
			await Message.encode(w, async (wr) => {
				await wr.u62(1n);
				await Namespace.encode(wr, Path.from("alice.hang"));
				const params = intoParams(advert);
				params.bytes.set(0x03n, new Uint8Array([0x03, 0x00, 0xaa])); // AUTHORIZATION_TOKEN: USE_VALUE, type 0
				await params.encode(wr, version);
			});
		},
	};

	const decoded = await PublishNamespace.decode(reader(await encode(withToken)), VERSION, CLUSTER);
	expect(decoded.params.cluster).toEqual(advert);
});

test("Cluster: a repeated AUTHORIZATION_TOKEN refuses only the advertisement", async () => {
	// USE_VALUE, type 0, value 0xAA, twice. A repeated key codes as a delta of 0.
	const tokens = [2, 0x03, 3, 0x03, 0x00, 0xaa, 0, 3, 0x03, 0x00, 0xaa];
	const frame = (body: number[]) => new Uint8Array([0, body.length, ...body]);

	// Request ID 1 for namespace "a". The repeat is refused before the missing HOP_PATH is.
	const advert = PublishNamespace.decode(reader(frame([1, 1, 1, 0x61, ...tokens])), VERSION, CLUSTER);
	await expect(advert).rejects.toThrow(MalformedAuthToken);
	await expect(advert).rejects.toMatchObject({ requestId: 1n });

	const update = PublishNamespaceUpdate.decode(reader(frame([1, ...tokens])), VERSION);
	await expect(update).rejects.toThrow(MalformedAuthToken);
});

test("Cluster: every malformed PUBLISH_NAMESPACE update is a protocol violation", async () => {
	const valid = await encode(new PublishNamespaceUpdate({ requestId: 1n, update: { cost: 2n } }));
	const trailing = new Uint8Array(valid.byteLength + 1);
	trailing.set(valid);
	trailing[1] += 1; // Include the extra byte in the message body's declared size.

	const malformed = [
		valid.slice(0, 1), // Truncated size prefix.
		valid.slice(0, -1), // Truncated body.
		new Uint8Array([0, 1, 0xff]), // Truncated request ID.
		trailing,
	];

	for (const bytes of malformed) {
		await expect(PublishNamespaceUpdate.decode(reader(bytes), VERSION)).rejects.toThrow(ProtocolViolation);
	}

	// The message did not exist before draft-16.
	await expect(
		PublishNamespaceUpdate.decode(reader(new Uint8Array([0, 0]), Version.DRAFT_15), Version.DRAFT_15),
	).rejects.toThrow(ProtocolViolation);
});

test("Cluster: a draft-16 PUBLISH_NAMESPACE update names the advertisement", async () => {
	const version = Version.DRAFT_16;
	const update = new PublishNamespaceUpdate({ requestId: 2n, namespaceRequestId: 0n, update: {} });
	const decoded = await PublishNamespaceUpdate.decode(reader(await encode(update, version), version), version);
	expect(decoded.requestId).toBe(2n);
	expect(decoded.namespaceRequestId).toBe(0n);

	// A repeated token refuses the update, keyed by its own Request ID.
	const tokens = [2, 0x03, 3, 0x03, 0x00, 0xaa, 0, 3, 0x03, 0x00, 0xaa];
	const body = [2, 0, ...tokens];
	const repeated = PublishNamespaceUpdate.decode(reader(new Uint8Array([0, body.length, ...body]), version), version);
	await expect(repeated).rejects.toThrow(MalformedAuthToken);
	await expect(repeated).rejects.toMatchObject({ requestId: 2n });
});

test("Cluster: a reset while decoding an update stays stream-local", async () => {
	const reset = Object.assign(new Error("reset"), {
		source: "stream" as const,
		streamErrorCode: StreamCode.Cancel,
	});
	const stream = new ReadableStream<Uint8Array>({
		start(controller) {
			controller.error(reset);
		},
	});

	const err = await PublishNamespaceUpdate.decode(new Reader(stream, undefined, VERSION), VERSION).catch(
		(err: unknown) => err,
	);
	expect(err).toBeInstanceOf(StreamError);
	expect((err as StreamError).code).toBe(StreamCode.Cancel);
});

test("Cluster: a transport ending while decoding an update stays transport-local", async () => {
	const ended = Object.assign(new Error("session ended"), {
		source: "session" as const,
		streamErrorCode: null,
	});
	const stream = new ReadableStream<Uint8Array>({
		start(controller) {
			controller.error(ended);
		},
	});

	const err = await PublishNamespaceUpdate.decode(new Reader(stream, undefined, VERSION), VERSION).catch(
		(err: unknown) => err,
	);
	expect(err).toBe(ended);
});

test("Cluster: NAMESPACE grows a parameters field only once negotiated", async () => {
	const advert = { hops: hops(7n), cost: 0n };
	const extended = await encode(
		new SubscribeNamespaceEntry({ suffix: Path.from("alice.hang"), params: { cluster: advert } }),
	);
	const base = await encode(new SubscribeNamespaceEntry({ suffix: Path.from("alice.hang") }));

	// The base form has no Parameters field at all, so it is strictly shorter.
	expect(base.length).toBeLessThan(extended.length);

	const decoded = await SubscribeNamespaceEntry.decode(reader(extended), VERSION, CLUSTER);
	expect(decoded.suffix).toBe(Path.from("alice.hang"));
	expect(decoded.params?.cluster).toEqual(advert);

	expect((await SubscribeNamespaceEntry.decode(reader(base), VERSION)).params).toBeUndefined();

	// Reading the extended form as the base one leaves the parameters behind rather than
	// silently absorbing them.
	await expect(SubscribeNamespaceEntry.decode(reader(extended), VERSION)).rejects.toThrow();

	// The base form on a negotiated session ends before the parameters the peer owed us,
	// which is the same violation as sending them without a HOP_PATH.
	await expect(SubscribeNamespaceEntry.decode(reader(base), VERSION, CLUSTER)).rejects.toThrow(ProtocolViolation);
});

import { expect, test } from "bun:test";
import { ProtocolViolation } from "../error.ts";
import { type Origin, OriginSchema, UNKNOWN_ORIGIN } from "../hop.ts";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import * as Cluster from "./cluster.ts";
import { Parameters, SetupOption, SetupOptions } from "./parameters.ts";
import { PublishNamespace } from "./publish_namespace.ts";
import { SubscribeNamespaceEntry } from "./subscribe_namespace.ts";
import { type IetfVersion, Version } from "./version.ts";

const VERSION = Version.DRAFT_19;

function origin(id: bigint): Origin {
	return OriginSchema.parse(id);
}

function hops(...ids: bigint[]): Origin[] {
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

function reader(bytes: Uint8Array, version: IetfVersion = VERSION): Reader {
	return new Reader(undefined, bytes, version);
}

test("Cluster: the registry code points and their parity", () => {
	// Other implementations key on these, so a typo is invisible against ourselves and
	// fatal against anyone else. The parity is load-bearing too: odd values are
	// length-prefixed byte strings, even ones bare varints.
	expect(SetupOption.RelayHops).toBe(0x40b55n);
	expect(SetupOption.RelayCost).toBe(0x40b56n);
	expect(SetupOption.RelayHops % 2n).toBe(1n);
	expect(SetupOption.RelayCost % 2n).toBe(0n);
});

test("Cluster: the hop path carries no count of its own", async () => {
	// The entries fill the parameter length; a count would make us unreadable to every
	// other implementation.
	const params = Cluster.intoParams({ hops: hops(1n, 2n, 3n), cost: 0n });
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
	const encoded = await encode(Cluster.intoParams(advert));
	const decoded = await Parameters.decode(reader(encoded), VERSION);
	expect(Cluster.fromParams(decoded)).toEqual(advert);
});

test("Cluster: a zero route cost is absent from the wire", async () => {
	const free = await encode(Cluster.intoParams({ hops: hops(7n), cost: 0n }));
	const priced = await encode(Cluster.intoParams({ hops: hops(7n), cost: 1n }));
	expect(free.length).toBeLessThan(priced.length);

	// Absent means 0, so it comes back as one.
	const decoded = await Parameters.decode(reader(free), VERSION);
	expect(Cluster.fromParams(decoded).cost).toBe(0n);
});

test("Cluster: a repeated zero is not a loop", async () => {
	const advert = { hops: hops(0n, 5n, 0n), cost: 0n };
	const encoded = await encode(Cluster.intoParams(advert));
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
	expect(() => Cluster.intoParams({ hops: hops(4n, 8n, 4n), cost: 0n })).toThrow();

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
	expect(() => Cluster.intoParams({ hops: long, cost: 0n })).toThrow();
});

test("Cluster: the setup options round trip", async () => {
	for (const declared of [origin(42n), UNKNOWN_ORIGIN]) {
		const params = new SetupOptions();
		Cluster.intoSetup(params, declared, VERSION);

		const encoded = await encode(params);
		const decoded = await SetupOptions.decode(reader(encoded), VERSION);
		expect(Cluster.fromSetup(decoded, VERSION)).toBe(declared);
	}

	// A peer that declared nothing did not negotiate.
	expect(Cluster.fromSetup(new SetupOptions(), VERSION)).toBeUndefined();
});

test("Cluster: the setup options are skipped before draft-17", () => {
	// Draft-14..16 exchange SETUP over the legacy control stream, which this extension
	// does not cover; we neither send nor read the options there.
	for (const version of [Version.DRAFT_14, Version.DRAFT_15, Version.DRAFT_16]) {
		expect(Cluster.supported(version)).toBe(false);

		const params = new SetupOptions();
		Cluster.intoSetup(params, origin(42n), version);
		expect(params.getBytes(SetupOption.RelayHops)).toBeUndefined();

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
	expect(Cluster.negotiated({ self, peer: UNKNOWN_ORIGIN })).toBe(true);
	expect(Cluster.advertise({ self, peer: origin(9n) })).toEqual({ hops: [self], cost: 0n });
});

test("Cluster: a loop is detected by any non-zero id", () => {
	const advert = { hops: hops(0n, 5n), cost: 0n };

	// A receiver whose own id is 0 cannot detect loops through itself.
	expect(Cluster.loops(advert, UNKNOWN_ORIGIN)).toBe(false);
	expect(Cluster.loops(advert, origin(5n))).toBe(true);
	expect(Cluster.loops(advert, origin(6n))).toBe(false);
});

test("Cluster: PUBLISH_NAMESPACE carries the parameters only once negotiated", async () => {
	const advert = { hops: hops(7n, 9n), cost: 5n };
	const msg = new PublishNamespace({ requestId: 1n, trackNamespace: Path.from("alice.hang"), cluster: advert });
	const encoded = await encode(msg);

	const decoded = await PublishNamespace.decode(reader(encoded), VERSION, true);
	expect(decoded.trackNamespace).toBe(Path.from("alice.hang"));
	expect(decoded.cluster).toEqual(advert);

	// The message always has a Parameters field, so a session that negotiated nothing
	// reads the same bytes and ignores what it did not ask for.
	expect((await PublishNamespace.decode(reader(encoded), VERSION)).cluster).toBeUndefined();

	// A negotiated session that omits HOP_PATH is a protocol violation, which the session
	// dispatch closes over rather than losing only the stream.
	const bare = new PublishNamespace({ requestId: 1n, trackNamespace: Path.from("alice.hang") });
	await expect(PublishNamespace.decode(reader(await encode(bare)), VERSION, true)).rejects.toThrow(ProtocolViolation);
});

test("Cluster: NAMESPACE grows a parameters field only once negotiated", async () => {
	const advert = { hops: hops(7n), cost: 0n };
	const extended = await encode(new SubscribeNamespaceEntry({ suffix: Path.from("alice.hang"), cluster: advert }));
	const base = await encode(new SubscribeNamespaceEntry({ suffix: Path.from("alice.hang") }));

	// The base form has no Parameters field at all, so it is strictly shorter.
	expect(base.length).toBeLessThan(extended.length);

	const decoded = await SubscribeNamespaceEntry.decode(reader(extended), VERSION, true);
	expect(decoded.suffix).toBe(Path.from("alice.hang"));
	expect(decoded.cluster).toEqual(advert);

	expect((await SubscribeNamespaceEntry.decode(reader(base), VERSION)).cluster).toBeUndefined();

	// Reading the extended form as the base one leaves the parameters behind rather than
	// silently absorbing them.
	await expect(SubscribeNamespaceEntry.decode(reader(extended), VERSION)).rejects.toThrow();

	// The base form on a negotiated session ends before the parameters the peer owed us,
	// which is the same violation as sending them without a HOP_PATH.
	await expect(SubscribeNamespaceEntry.decode(reader(base), VERSION, true)).rejects.toThrow(ProtocolViolation);
});

import { expect, test } from "bun:test";
import { Datagram } from "./datagram.ts";
import { Version } from "./version.ts";

const enc = new TextEncoder();
const dec = new TextDecoder();

test("invalid datagram types are refused", async () => {
	// 0x0 is the only type.
	for (const bytes of [[], [0x80], [1, 1, 2, 3], [2, 1, 2, 3], new Uint8Array(9).fill(0xff)]) {
		await expect(Datagram.decode(new Uint8Array(bytes), Version.DRAFT_07)).rejects.toThrow();
	}
});

test("datagram body round-trips", async () => {
	for (const version of [Version.DRAFT_05, Version.DRAFT_06, Version.DRAFT_07]) {
		const dg = new Datagram(1n, 42n, 1000n, enc.encode("hello"));
		const decoded = await Datagram.decode(dg.encode(version), version);
		expect(decoded.subscribe).toBe(1n);
		expect(decoded.sequence).toBe(42n);
		expect(decoded.timestamp).toBe(1000n);
		expect(dec.decode(decoded.payload)).toBe("hello");
	}
});

test("datagram body has no inner length prefix", () => {
	const dg = new Datagram(1n, 2n, 3n, enc.encode("world"));
	const body = dg.encode(Version.DRAFT_06);
	// Three single-byte varints (values < 64) followed by the raw 5-byte payload.
	expect(body.byteLength).toBe(8);
	expect(dec.decode(body.slice(3))).toBe("world");
});

test("datagram body varints follow the version's encoding", () => {
	// 100 takes the two-byte QUIC form on lite-06 and one leading-ones byte on lite-07.
	const dg = new Datagram(100n, 100n, 100n, new Uint8Array());
	expect([...dg.encode(Version.DRAFT_06)]).toEqual([0x40, 0x64, 0x40, 0x64, 0x40, 0x64]);
	expect([...dg.encode(Version.DRAFT_07)]).toEqual([0, 0x64, 0x64, 0x64]);
});

test("datagram body round-trips an empty payload", async () => {
	const dg = new Datagram(0n, 0n, 0n, new Uint8Array());
	const decoded = await Datagram.decode(dg.encode(Version.DRAFT_07), Version.DRAFT_07);
	expect(decoded.payload.byteLength).toBe(0);
});

test("a scale-less lite-07 datagram carries its bare timestamp at full width", async () => {
	const far = 1n << 60n;
	const dg = new Datagram(1n, far, far, enc.encode("world"));
	const decoded = await Datagram.decode(dg.encode(Version.DRAFT_07), Version.DRAFT_07);
	expect([decoded.sequence, decoded.timestamp]).toEqual([far, far]);
	expect(dec.decode(decoded.payload)).toBe("world");
});

test("lite-07 datagram requests match Rust's size-prefixed fields", async () => {
	const { Datagrams, DatagramsInfo, DatagramsUpdate } = await import("./datagram.ts");
	const { Reader, Writer } = await import("../stream.ts");
	const Path = await import("../path.ts");
	const { Timescale } = await import("../time.ts");
	const request = new Datagrams({ id: 7n, broadcast: Path.from("room/alice"), track: "audio", priority: -3 });
	const body = await Writer.collect(Version.DRAFT_07, (writer) => request.encode(writer, Version.DRAFT_07));
	// Id, broadcast, an empty unminted pin, Node ID 0, Route Depth 0, track, priority.
	expect([...body]).toEqual([23, 7, 10, ...enc.encode("room/alice"), 0, 0, 0, 0, 5, ...enc.encode("audio"), 125]);
	const decoded = await Datagrams.decode(new Reader(undefined, body, Version.DRAFT_07), Version.DRAFT_07);
	expect(decoded).toEqual(request);
	for (const timescale of [undefined, Timescale.MILLI]) {
		const info = new DatagramsInfo({ priority: 2, timescale });
		const body = await Writer.collect(Version.DRAFT_07, (writer) => info.encode(writer, Version.DRAFT_07));
		expect(await DatagramsInfo.decode(new Reader(undefined, body, Version.DRAFT_07), Version.DRAFT_07)).toEqual(
			info,
		);
	}
	const update = new DatagramsUpdate(9);
	const updated = await Writer.collect(Version.DRAFT_07, (writer) => update.encode(writer, Version.DRAFT_07));
	expect([...updated]).toEqual([1, 137]);
	expect(
		await DatagramsUpdate.decodeMaybe(new Reader(undefined, updated, Version.DRAFT_07), Version.DRAFT_07),
	).toEqual(update);
	await expect(
		Writer.collect(Version.DRAFT_06, (writer) => request.encode(writer, Version.DRAFT_06)),
	).rejects.toThrow("require lite-07");
});

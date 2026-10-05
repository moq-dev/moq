import { expect, test } from "bun:test";
import { Datagram } from "./datagram.ts";
import { Version } from "./version.ts";

const enc = new TextEncoder();
const dec = new TextDecoder();

test("datagram body round-trips", async () => {
	for (const version of [Version.DRAFT_06, Version.DRAFT_07]) {
		const dg = new Datagram(7n, 42, 1000, enc.encode("hello"));
		const decoded = await Datagram.decode(dg.encode(version), version);
		expect(decoded.subscribe).toBe(7n);
		expect(decoded.sequence).toBe(42);
		expect(decoded.timestamp).toBe(1000);
		expect(dec.decode(decoded.payload)).toBe("hello");
	}
});

test("datagram body has no inner length prefix", () => {
	const dg = new Datagram(1n, 2, 3, enc.encode("world"));
	const body = dg.encode(Version.DRAFT_06);
	// Three single-byte varints (values < 64) followed by the raw 5-byte payload.
	expect(body.byteLength).toBe(8);
	expect(dec.decode(body.slice(3))).toBe("world");
});

test("datagram body varints follow the version's encoding", () => {
	// 100 takes the two-byte QUIC form on lite-06 and one leading-ones byte on lite-07.
	const dg = new Datagram(100n, 100, 100, new Uint8Array());
	expect([...dg.encode(Version.DRAFT_06)]).toEqual([0x40, 0x64, 0x40, 0x64, 0x40, 0x64]);
	expect([...dg.encode(Version.DRAFT_07)]).toEqual([0x64, 0x64, 0x64]);
});

test("datagram body round-trips an empty payload", async () => {
	const dg = new Datagram(0n, 0, 0, new Uint8Array());
	const decoded = await Datagram.decode(dg.encode(Version.DRAFT_07), Version.DRAFT_07);
	expect(decoded.payload.byteLength).toBe(0);
});

import { expect, test } from "bun:test";
import assert from "node:assert/strict";
import { Datagram } from "./datagram.ts";
import { Version } from "./version.ts";

const enc = new TextEncoder();
const dec = new TextDecoder();

test("lite-07 padding is discarded before looking up a subscription", async () => {
	for (const body of [
		new Uint8Array([1]),
		new Uint8Array([1, ...new Uint8Array(1199).fill(0xff)]),
		new Uint8Array([0x80, 1, 0xff]),
	]) {
		expect(
			await Datagram.decode(body, Version.DRAFT_07, () => {
				throw new Error("padding must not look up a subscription");
			}),
		).toBeUndefined();
	}
});

test("invalid datagram types never reach a subscription", async () => {
	for (const bytes of [[], [0x80], [2, 1, 2, 3], new Uint8Array(9).fill(0xff)]) {
		let lookedUp = false;
		await expect(
			Datagram.decode(new Uint8Array(bytes), Version.DRAFT_07, () => {
				lookedUp = true;
				throw new Error("invalid type must not look up a subscription");
			}),
		).rejects.toThrow();
		expect(lookedUp).toBe(false);
	}
});

test("datagram body round-trips", async () => {
	for (const version of [Version.DRAFT_05, Version.DRAFT_06, Version.DRAFT_07]) {
		const dg = new Datagram(1n, 42, 1000, enc.encode("hello"));
		const decoded = await Datagram.decode(dg.encode(version), version, () => ({ timed: true }));
		assert(decoded);
		expect(decoded.subscribe).toBe(1n);
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
	expect([...dg.encode(Version.DRAFT_07)]).toEqual([0, 0x64, 0x64, 0x64]);
});

test("datagram body round-trips an empty payload", async () => {
	const dg = new Datagram(0n, 0, 0, new Uint8Array());
	const decoded = await Datagram.decode(dg.encode(Version.DRAFT_07), Version.DRAFT_07, () => ({ timed: true }));
	assert(decoded);
	expect(decoded.payload.byteLength).toBe(0);
});

test("an untimed datagram body carries no timestamp", async () => {
	const dg = new Datagram(1n, 2, undefined, enc.encode("world"));
	const body = dg.encode(Version.DRAFT_07);
	expect(body.byteLength).toBe(8);
	const decoded = await Datagram.decode(body, Version.DRAFT_07, (subscribe) => {
		expect(subscribe).toBe(1n);
		return { timed: false };
	});
	assert(decoded);
	expect(decoded.sequence).toBe(2);
	expect(decoded.timestamp).toBeUndefined();
	expect(dec.decode(decoded.payload)).toBe("world");
});

test("an untimed datagram body is refused before lite-07", () => {
	const dg = new Datagram(1n, 2, undefined, enc.encode("world"));
	for (const version of [Version.DRAFT_05, Version.DRAFT_06]) {
		expect(() => dg.encode(version)).toThrow("has no untimed datagram");
	}
});

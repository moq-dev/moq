import { expect, test } from "bun:test";
import { Datagram } from "./datagram.ts";

const enc = new TextEncoder();
const dec = new TextDecoder();

test("datagram body round-trips", async () => {
	const dg = new Datagram(7n, 42, 1000, enc.encode("hello"));
	const decoded = await Datagram.decode(dg.encode());
	expect(decoded.subscribe).toBe(7n);
	expect(decoded.sequence).toBe(42);
	expect(decoded.timestamp).toBe(1000);
	expect(dec.decode(decoded.payload)).toBe("hello");
});

test("datagram body has no inner length prefix", () => {
	const dg = new Datagram(1n, 2, 3, enc.encode("world"));
	const body = dg.encode();
	// Three single-byte varints (values < 64) followed by the raw 5-byte payload.
	expect(body.byteLength).toBe(8);
	expect(dec.decode(body.slice(3))).toBe("world");
});

test("datagram body round-trips an empty payload", async () => {
	const dg = new Datagram(0n, 0, 0, new Uint8Array());
	const decoded = await Datagram.decode(dg.encode());
	expect(decoded.payload.byteLength).toBe(0);
});

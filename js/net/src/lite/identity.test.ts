import { expect, test } from "bun:test";
import { ProtocolViolation } from "../error.ts";
import * as Identity from "../identity.ts";
import * as Mint from "../mint.ts";
import { Reader, Writer } from "../stream.ts";
import { decodePin, decodeRoute, encodePin, encodeRoute } from "./identity.ts";
import { Version } from "./version.ts";

async function encoded(f: (w: Writer) => Promise<void>): Promise<Uint8Array> {
	const written: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void written.push(new Uint8Array(chunk)) }),
		Version.DRAFT_07,
	);
	await f(writer);
	writer.close();
	await writer.closed;
	return new Uint8Array(written.flatMap((chunk) => [...chunk]));
}

// A leading U+FEFF is identity text like any other, so it survives the wire as Rust keeps it.
test("an identity keeps a leading byte order mark", async () => {
	for (const text of ["﻿camera", "﻿"]) {
		const identity = Identity.parse(text);
		const pin = await encoded((w) => encodePin(w, Version.DRAFT_07, identity));
		expect(await decodePin(new Reader(undefined, pin, Version.DRAFT_07), Version.DRAFT_07)).toBe(identity);
		const route = await encoded((w) => encodeRoute(w, Version.DRAFT_07, identity, 3n));
		expect(await decodeRoute(new Reader(undefined, route, Version.DRAFT_07), Version.DRAFT_07)).toEqual({
			identity,
			epoch: 3n,
		});
	}
});

// The flag, not the text, says a session minted an identity: each round-trips as itself, and a
// named identity that looks like the old `~` mark stays named.
test("a minted identity keeps its flag, and text never builds one", async () => {
	const minted = Mint.fresh();
	const named = Identity.parse(Identity.text(minted));
	expect(Identity.isMinted(minted)).toBe(true);
	expect(Identity.isMinted(named)).toBe(false);
	expect(named).not.toBe(minted);
	expect(Identity.isMinted(Identity.parse("~named"))).toBe(false);
	// U+10000 is the surrogate pair D800 DC00: well-formed text that starts with a surrogate.
	for (const text of ["\u{10000}", "\u{10000}x", "\u{FFFF}"]) {
		const astral = Identity.parse(text);
		expect(Identity.isMinted(astral)).toBe(false);
		expect(Identity.text(astral)).toBe(text);
	}
	// A lone surrogate is ill-formed text, which UTF-8 cannot carry.
	expect(() => Identity.parse("\uDC00x")).toThrow();
	// Minting interns by text, so equal minted identities are the same object.
	expect(Mint.mint(Identity.text(minted))).toBe(minted);
	expect(`${minted}`).toBe(Identity.text(minted));
	// Nothing outside the session can build one, even holding its class.
	const Class = Object.getPrototypeOf(minted).constructor;
	expect(() => new Class(Symbol("mint"), "forged")).toThrow();
	expect(Identity.isMinted(Object.create(Class.prototype))).toBe(false);
	for (const identity of [minted, named]) {
		const pin = await encoded((w) => encodePin(w, Version.DRAFT_07, identity));
		expect(await decodePin(new Reader(undefined, pin, Version.DRAFT_07), Version.DRAFT_07)).toBe(identity);
		const route = await encoded((w) => encodeRoute(w, Version.DRAFT_07, identity, 0n));
		expect((await decodeRoute(new Reader(undefined, route, Version.DRAFT_07), Version.DRAFT_07)).identity).toBe(
			identity,
		);
	}
});

test("a pin refuses a bad minted flag", async () => {
	const decode = async (text: string, flag: number) => {
		const pin = await encoded(async (w) => {
			const bytes = new TextEncoder().encode(text);
			await w.u53(bytes.byteLength);
			await w.write(bytes);
			await w.u8(flag);
		});
		return decodePin(new Reader(undefined, pin, Version.DRAFT_07), Version.DRAFT_07);
	};
	await expect(decode("a", 2)).rejects.toThrow();
	// No identity is never minted.
	await expect(decode("", 1)).rejects.toThrow();
	expect(await decode("", 0)).toBeUndefined();
});

// 256 UTF-8 bytes is the ceiling, counted in bytes rather than characters, in a pin and a route
// alike; a peer exceeding it is a PROTOCOL_VIOLATION.
test("an identity holds at most 256 UTF-8 bytes", async () => {
	const full = "é".repeat(128);
	const over = `${full}a`;
	expect(Identity.text(Identity.parse(full))).toBe(full);
	expect(() => Identity.parse(over)).toThrow(RangeError);
	expect(() => Identity.parse("a".repeat(257))).toThrow(RangeError);
	const raw = (text: string) =>
		encoded(async (w) => {
			const bytes = new TextEncoder().encode(text);
			await w.u53(bytes.byteLength);
			await w.write(bytes);
			await w.bool(false);
			await w.u62(0n);
		});
	for (const [text, ok] of [
		[full, true],
		[over, false],
	] as const) {
		const bytes = await raw(text);
		const pin = () => decodePin(new Reader(undefined, bytes, Version.DRAFT_07), Version.DRAFT_07);
		const route = () => decodeRoute(new Reader(undefined, bytes, Version.DRAFT_07), Version.DRAFT_07);
		if (ok) {
			expect((await pin())?.toString()).toBe(text);
			expect((await route()).identity?.toString()).toBe(text);
		} else {
			await expect(pin()).rejects.toBeInstanceOf(ProtocolViolation);
			await expect(route()).rejects.toBeInstanceOf(ProtocolViolation);
		}
	}
});

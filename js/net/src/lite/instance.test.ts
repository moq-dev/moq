import { expect, test } from "bun:test";
import * as Instance from "../instance.ts";
import * as Mint from "../mint.ts";
import { Reader, Writer } from "../stream.ts";
import { decodePin, decodeRoute, encodePin, encodeRoute } from "./instance.ts";
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

// A leading U+FEFF is instance text like any other, so it survives the wire as Rust keeps it.
test("an instance keeps a leading byte order mark", async () => {
	for (const text of ["﻿camera", "﻿"]) {
		const instance = Instance.parse(text);
		const pin = await encoded((w) => encodePin(w, Version.DRAFT_07, instance));
		expect(await decodePin(new Reader(undefined, pin, Version.DRAFT_07), Version.DRAFT_07)).toBe(instance);
		const route = await encoded((w) => encodeRoute(w, Version.DRAFT_07, instance, 3n));
		expect(await decodeRoute(new Reader(undefined, route, Version.DRAFT_07), Version.DRAFT_07)).toEqual({
			instance,
			epoch: 3n,
		});
	}
});

// The flag, not the text, says a session minted an instance: each round-trips as itself, and a
// named instance that looks like the old `~` mark stays named.
test("a minted instance keeps its flag, and text never builds one", async () => {
	const minted = Mint.fresh();
	const named = Instance.parse(Instance.text(minted));
	expect(Instance.isMinted(minted)).toBe(true);
	expect(Instance.isMinted(named)).toBe(false);
	expect(named).not.toBe(minted);
	expect(Instance.isMinted(Instance.parse("~named"))).toBe(false);
	expect(() => Instance.parse(minted)).toThrow();
	for (const instance of [minted, named]) {
		const pin = await encoded((w) => encodePin(w, Version.DRAFT_07, instance));
		expect(await decodePin(new Reader(undefined, pin, Version.DRAFT_07), Version.DRAFT_07)).toBe(instance);
		const route = await encoded((w) => encodeRoute(w, Version.DRAFT_07, instance, 0n));
		expect((await decodeRoute(new Reader(undefined, route, Version.DRAFT_07), Version.DRAFT_07)).instance).toBe(
			instance,
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
	// No instance is never minted.
	await expect(decode("", 1)).rejects.toThrow();
	expect(await decode("", 0)).toBeUndefined();
});

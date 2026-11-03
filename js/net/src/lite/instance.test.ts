import { expect, test } from "bun:test";
import * as Instance from "../instance.ts";
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

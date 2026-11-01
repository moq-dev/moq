import { expect, test } from "bun:test";
import * as Identity from "../identity.ts";
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

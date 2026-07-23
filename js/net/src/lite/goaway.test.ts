import { expect, test } from "bun:test";
import { Reader, Writer } from "../stream.ts";
import { Goaway } from "./goaway.ts";
import { Version } from "./version.ts";

function concat(chunks: Uint8Array[]): Uint8Array {
	const total = chunks.reduce((sum, c) => sum + c.byteLength, 0);
	const out = new Uint8Array(total);
	let offset = 0;
	for (const c of chunks) {
		out.set(c, offset);
		offset += c.byteLength;
	}
	return out;
}

async function encode(msg: Goaway, version: Version): Promise<Uint8Array> {
	const written: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void written.push(new Uint8Array(chunk)) }),
	);
	await msg.encode(writer, version);
	writer.close();
	await writer.closed;
	return concat(written);
}

async function decode(bytes: Uint8Array, version: Version): Promise<Goaway> {
	const reader = new Reader(undefined, bytes);
	return await Goaway.decode(reader, version);
}

test("Goaway: round trip on draft-04", async () => {
	const msg = new Goaway("https://new-relay.example/session");

	const encoded = await encode(msg, Version.DRAFT_04);
	const decoded = await decode(encoded, Version.DRAFT_04);

	expect(decoded.uri).toBe("https://new-relay.example/session");
});

test("Goaway: empty URI on draft-04", async () => {
	const msg = new Goaway("");

	const encoded = await encode(msg, Version.DRAFT_04);
	const decoded = await decode(encoded, Version.DRAFT_04);

	expect(decoded.uri).toBe("");
});

test("Goaway: round trip on draft-05", async () => {
	const msg = new Goaway("moql://relay.example/migrate");

	const encoded = await encode(msg, Version.DRAFT_05);
	const decoded = await decode(encoded, Version.DRAFT_05);

	expect(decoded.uri).toBe("moql://relay.example/migrate");
});

test("Goaway: rejects draft-01", async () => {
	const msg = new Goaway("https://example.com");
	await expect(encode(msg, Version.DRAFT_01)).rejects.toThrow(/not supported/);
});

test("Goaway: rejects draft-02", async () => {
	const msg = new Goaway("https://example.com");
	await expect(encode(msg, Version.DRAFT_02)).rejects.toThrow(/not supported/);
});

test("Goaway: rejects draft-03", async () => {
	const msg = new Goaway("https://example.com");
	await expect(encode(msg, Version.DRAFT_03)).rejects.toThrow(/not supported/);
});

test("Goaway: decode rejects draft-01", async () => {
	// Encode as v04 (valid), then try to decode as v01 (rejected at guard)
	const msg = new Goaway("https://example.com");
	const bytes = await encode(msg, Version.DRAFT_04);
	await expect(decode(bytes, Version.DRAFT_01)).rejects.toThrow(/not supported/);
});

test("Goaway: unicode URI", async () => {
	const msg = new Goaway("https://リレー.example/セッション");

	const encoded = await encode(msg, Version.DRAFT_04);
	const decoded = await decode(encoded, Version.DRAFT_04);

	expect(decoded.uri).toBe("https://リレー.example/セッション");
});

test("Goaway: URI at the 8192-byte cap round trips", async () => {
	const msg = new Goaway("a".repeat(8192));

	const encoded = await encode(msg, Version.DRAFT_04);
	const decoded = await decode(encoded, Version.DRAFT_04);

	expect(decoded.uri.length).toBe(8192);
});

test("Goaway: rejects URI over the 8192-byte cap", async () => {
	const msg = new Goaway("a".repeat(8193));
	const encoded = await encode(msg, Version.DRAFT_04);
	await expect(decode(encoded, Version.DRAFT_04)).rejects.toThrow(/8,192/);
});

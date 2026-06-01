import { expect, test } from "bun:test";
import { Compression } from "../compression.ts";
import { Reader, Writer } from "../stream.ts";
import { DEFAULT_CACHE_MS } from "../track.ts";
import { SubscribeOk } from "./subscribe.ts";
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

async function roundtrip(version: Version, ok: SubscribeOk): Promise<SubscribeOk> {
	const written: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void written.push(new Uint8Array(chunk)) }),
	);
	await ok.encode(writer, version);
	writer.close();
	await writer.closed;

	const reader = new Reader(undefined, concat(written));
	return SubscribeOk.decode(reader, version);
}

test("SubscribeOk: compression and cache round-trip on draft-05", async () => {
	const ok = new SubscribeOk({
		priority: 7,
		ordered: true,
		maxLatency: 250,
		startGroup: 3,
		compression: Compression.Deflate,
		cache: 10000,
	});

	const got = await roundtrip(Version.DRAFT_05_WIP, ok);
	expect(got.compression).toBe(Compression.Deflate);
	expect(got.cache).toBe(10000);
	expect(got.priority).toBe(7);
	expect(got.ordered).toBe(true);
	expect(got.startGroup).toBe(3);
});

test("SubscribeOk: compression and cache fields are absent before draft-05", async () => {
	// draft-04 has no compression/cache varints on the wire, so they decode to defaults.
	const ok = new SubscribeOk({ priority: 7, compression: Compression.Deflate, cache: 10000 });
	const got = await roundtrip(Version.DRAFT_04, ok);
	expect(got.compression).toBe(Compression.None);
	expect(got.cache).toBe(DEFAULT_CACHE_MS);
});

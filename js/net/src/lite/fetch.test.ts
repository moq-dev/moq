import { expect, test } from "bun:test";
import { HopSchema } from "../hop.ts";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import { Fetch, FetchOk } from "./fetch.ts";
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

async function encode(version: Version, fetch: Fetch): Promise<Uint8Array> {
	const written: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void written.push(new Uint8Array(chunk)) }),
	);
	await fetch.encode(writer, version);
	writer.close();
	await writer.closed;
	return concat(written);
}

async function roundtrip(version: Version, fetch: Fetch): Promise<Fetch> {
	const reader = new Reader(undefined, await encode(version, fetch));
	return Fetch.decode(reader, version);
}

function sample(): Fetch {
	return new Fetch({ broadcast: Path.from("room/1"), track: "video", priority: 3, group: 42 });
}

test("Fetch round-trips on draft-03/04/05", async () => {
	for (const version of [Version.DRAFT_03, Version.DRAFT_04, Version.DRAFT_05]) {
		const got = await roundtrip(version, sample());
		expect(got.broadcast).toBe(Path.from("room/1"));
		expect(got.track).toBe("video");
		expect(got.priority).toBe(3);
		expect(got.group).toBe(42);
	}
});

test("FetchOk names the origin on draft-07 only", async () => {
	const written: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void written.push(new Uint8Array(chunk)) }),
	);
	await new FetchOk(HopSchema.parse(42n)).encode(writer, Version.DRAFT_07);
	writer.close();
	await writer.closed;
	const buf = concat(written);
	expect(buf).toEqual(new Uint8Array([1, 42]));
	const got = await FetchOk.decode(new Reader(undefined, buf), Version.DRAFT_07);
	expect(got.origin).toBe(HopSchema.parse(42n));

	await expect(new FetchOk(HopSchema.parse(42n)).encode(writer, Version.DRAFT_06)).rejects.toThrow();
});

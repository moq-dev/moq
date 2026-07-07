import { expect, test } from "bun:test";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import { Fetch } from "./fetch.ts";
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
	return new Fetch(Path.from("room/1"), "video", 3, 42);
}

test("Fetch: round-trips", async () => {
	const got = await roundtrip(Version.DRAFT_05_WIP, sample());
	expect(got.group).toBe(42);
	expect(got.priority).toBe(3);
	expect(got.track).toBe("video");
});

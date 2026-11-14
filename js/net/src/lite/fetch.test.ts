import { expect, test } from "bun:test";

import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import { Fetch } from "./fetch.ts";
import * as Message from "./message.ts";
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
		version,
	);
	await fetch.encode(writer, version);
	writer.close();
	await writer.closed;
	return concat(written);
}

async function roundtrip(version: Version, fetch: Fetch): Promise<Fetch> {
	const reader = new Reader(undefined, await encode(version, fetch), version);
	return Fetch.decode(reader, version);
}

function sample(): Fetch {
	return new Fetch({ broadcast: Path.from("room/1"), track: "video", priority: 3, group: 42n });
}

test("Fetch round-trips on draft-03/04/05", async () => {
	for (const version of [Version.DRAFT_03, Version.DRAFT_04, Version.DRAFT_05]) {
		const got = await roundtrip(version, sample());
		expect(got.broadcast).toBe(Path.from("room/1"));
		expect(got.track).toBe("video");
		expect(got.priority).toBe(3);
		expect(got.group).toBe(42n);
	}
});

test("Fetch carries omitted and explicit maxDelay on draft-07", async () => {
	expect((await roundtrip(Version.DRAFT_07, sample())).maxDelay).toBeUndefined();

	for (const maxDelay of [0, 250, Number.MAX_SAFE_INTEGER]) {
		const got = await roundtrip(
			Version.DRAFT_07,
			new Fetch({ broadcast: Path.from("room/1"), track: "video", priority: 3, maxDelay, group: 42n }),
		);
		expect(got.maxDelay).toBe(maxDelay);
	}

	await expect(
		encode(
			Version.DRAFT_06,
			new Fetch({
				broadcast: Path.from("room/1"),
				track: "video",
				priority: 3,
				maxDelay: 250,
				group: 42n,
			}),
		),
	).rejects.toThrow("maxDelay");
});

test("Fetch decodes the optional delay offset before converting to milliseconds", async () => {
	const version = Version.DRAFT_07;
	for (const delay of [BigInt(Number.MAX_SAFE_INTEGER), BigInt(Number.MAX_SAFE_INTEGER) + 1n]) {
		// Encode the Rust peer's optional milliseconds directly, independently of Fetch.encode.
		const encoded = await Writer.collect(version, (writer) =>
			Message.encode(writer, async (w) => {
				await w.string("room/1");
				await w.u53(0); // no instance pin: empty and unminted
				await w.bool(false);
				await w.u62(0n); // no origin node
				await w.string("video");
				await w.u8(0);
				await w.u62(delay + 1n);
				await w.u62(42n);
				await w.u53(0); // start frame
				await w.u53(0); // start byte
				await w.u53(0); // through the end
			}),
		);
		const decoded = Fetch.decode(new Reader(undefined, encoded, version), version);
		if (delay === BigInt(Number.MAX_SAFE_INTEGER)) expect((await decoded).maxDelay).toBe(Number.MAX_SAFE_INTEGER);
		else await expect(decoded).rejects.toThrow(RangeError);
	}
	const unsafe = sample();
	unsafe.maxDelay = Number.MAX_SAFE_INTEGER + 1;
	await expect(encode(version, unsafe)).rejects.toThrow(RangeError);
});

test("Fetch carries a start byte only on draft-07, beside its start frame", async () => {
	const partway = () =>
		new Fetch({
			broadcast: Path.from("room/1"),
			track: "video",
			priority: 3,
			group: 42n,
			startFrame: 2,
			startByte: 1 << 20,
			endFrame: 5,
		});
	const got = await roundtrip(Version.DRAFT_07, partway());
	expect([got.startFrame, got.startByte, got.endFrame]).toEqual([2, 1 << 20, 5]);
	await expect(encode(Version.DRAFT_06, partway())).rejects.toThrow("start byte");
	expect((await roundtrip(Version.DRAFT_06, sample())).startByte).toBe(0);
});

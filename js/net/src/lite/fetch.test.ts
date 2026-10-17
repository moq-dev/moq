import { expect, test } from "bun:test";
import { Milli } from "@moq/clock";
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

test("Fetch carries omitted and explicit maxDelay on draft-07", async () => {
	expect((await roundtrip(Version.DRAFT_07, sample())).maxDelay).toBeUndefined();

	for (const maxDelay of [Milli(0), Milli(250)]) {
		const got = await roundtrip(
			Version.DRAFT_07,
			new Fetch({ broadcast: Path.from("room/1"), track: "video", priority: 3, maxDelay, group: 42 }),
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
				maxDelay: Milli(250),
				group: 42,
			}),
		),
	).rejects.toThrow("maxDelay");
});

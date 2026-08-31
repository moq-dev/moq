import { expect, test } from "bun:test";
import { Decoder } from "./decoder.ts";
import { Encoder } from "./encoder.ts";

// Shared cross-impl fixture, owned by the reference Rust crate. This file is the wire contract
// between the two implementations, so a change here is a format change and both suites move together.
const url = new URL("../../../../rs/moq-json/tests/window-vectors.json", import.meta.url);
const vectors = (await Bun.file(url).json()) as Array<{ name: string; first: boolean; frame: string }>;

const frames = new Map(vectors.map((v) => [v.name, v.frame]));
const text = (payload: Uint8Array) => new TextDecoder().decode(payload);

// Look up a vector by name, failing loudly rather than comparing against undefined if it is renamed.
function frame(name: string): string {
	const found = frames.get(name);
	if (found === undefined) throw new Error(`no vector named "${name}"`);
	return found;
}

// Encode one frame and acknowledge it, so the encoder does not resync on the next edit.
function commit(frame: { payload: Uint8Array; commit(): void } | undefined): string {
	if (!frame) throw new Error("expected a frame");
	const encoded = text(frame.payload);
	frame.commit();
	return encoded;
}

test("the encoder emits the shared frame bytes", () => {
	const encoder = new Encoder<{ n: number }>();

	// A fresh encoder opens a group with a header restating the window.
	expect(commit(encoder.push({ n: 0 }))).toBe('{"offset":0,"records":[{"n":0}]}');
	expect(commit(encoder.push({ n: 2 }))).toBe(frame("push"));
	expect(commit(encoder.pop(1))).toBe(frame("pop one"));
});

test("the encoder emits the shared group header bytes", () => {
	// opRatio 0 forces every edit to restate the window in a new group.
	const encoder = new Encoder<{ n: number }>({ opRatio: 0 });
	commit(encoder.push({ n: 0 }));
	expect(commit(encoder.push({ n: 1 }))).toBe(frame("group header from zero"));
});

test("the encoder emits the shared bounded checkpoint bytes", () => {
	const encoder = new Encoder<{ n: number }>({ opRatio: 0, checkpointRecords: 1 });
	commit(encoder.push({ n: 0 }));
	expect(commit(encoder.push({ n: 1 }))).toBe(frame("group header with a bounded checkpoint"));
});

for (const vector of vectors) {
	test(`vector decodes: ${vector.name}`, () => {
		const decoder = new Decoder<unknown>();
		const group = decoder.group();
		// Every group opens with a header, so position the decoder before a bare op.
		if (!vector.first) {
			group.decode(new TextEncoder().encode('{"offset":0,"records":[{"n":0},{"n":1},{"n":2}]}'));
			while (decoder.next());
		}

		expect(() => group.decode(new TextEncoder().encode(vector.frame))).not.toThrow();
	});
}

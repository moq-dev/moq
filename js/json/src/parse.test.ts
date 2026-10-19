import { expect, test } from "bun:test";
import { DuplicateKey } from "./error.ts";
import { parse } from "./parse.ts";
import { Decoder as SnapshotDecoder } from "./snapshot/decoder.ts";
import { Decoder as StreamDecoder } from "./stream/decoder.ts";
import { Decoder as WindowDecoder } from "./window/decoder.ts";

// Shared cross-impl fixture, owned by the reference Rust crate, so both refuse the same frames.
const url = new URL("../../../rs/moq-json/tests/duplicate-keys.json", import.meta.url);
const vectors = (await Bun.file(url).json()) as { refused: string[]; accepted: string[] };
const encode = (text: string) => new TextEncoder().encode(text);

test("parse refuses a repeated key and accepts its near misses", () => {
	for (const text of vectors.refused) expect(() => parse(encode(text)), text).toThrow(DuplicateKey);
	for (const text of vectors.accepted) expect(parse(encode(text)), text).toEqual(JSON.parse(text));
});

test("parse accepts nesting deeper than the call stack", () => {
	const depth = 100_000;
	const text = `${'[{"a":'.repeat(depth)}0${"}]".repeat(depth)}`;
	expect(() => parse(encode(text))).not.toThrow();
	expect(() => parse(encode(`${'[{"a":0,"a":'.repeat(depth)}0${"}]".repeat(depth)}`))).toThrow(DuplicateKey);
});

test("malformed JSON stays a syntax error", () => {
	expect(() => parse(encode('{"a":1,"b"'))).toThrow(SyntaxError);
	expect(() => parse(encode('{"a":1} {}'))).toThrow(SyntaxError);
});

test("snapshots and deltas refuse a repeated key", () => {
	for (const text of vectors.refused) {
		const decoder = new SnapshotDecoder();
		expect(() => decoder.snapshot(encode(text)), text).toThrow(DuplicateKey);
		decoder.snapshot(encode("{}"));
		expect(() => decoder.delta(encode(text)), text).toThrow(DuplicateKey);
	}
	for (const text of vectors.accepted) {
		const decoder = new SnapshotDecoder();
		decoder.snapshot(encode(text));
		decoder.delta(encode(text));
	}
});

test("stream records refuse a repeated key", () => {
	const decoder = new StreamDecoder();
	for (const text of vectors.refused) expect(() => decoder.decode(encode(text)), text).toThrow(DuplicateKey);
	for (const text of vectors.accepted) expect(decoder.decode(encode(text)), text).toEqual(JSON.parse(text));
});

test("window headers, records, and ops refuse a repeated key", () => {
	for (const record of vectors.refused) {
		const group = new WindowDecoder().group();
		const header = `{"offset":0,"records":[${record}]}`;
		expect(() => group.decode(encode(header)), header).toThrow(DuplicateKey);
		group.decode(encode('{"offset":0,"records":[]}'));
		const push = `{"push":${record}}`;
		expect(() => group.decode(encode(push)), push).toThrow(DuplicateKey);
	}
	for (const record of vectors.accepted) {
		const group = new WindowDecoder().group();
		group.decode(encode(`{"offset":0,"records":[${record}]}`));
		group.decode(encode(`{"push":${record}}`));
	}

	const group = new WindowDecoder().group();
	expect(() => group.decode(encode('{"offset":0,"offset":0,"records":[]}'))).toThrow(DuplicateKey);
	group.decode(encode('{"offset":0,"records":[]}'));
	expect(() => group.decode(encode('{"pop":0,"pop":0}'))).toThrow(DuplicateKey);
});

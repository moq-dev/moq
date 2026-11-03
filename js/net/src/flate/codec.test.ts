import { expect, spyOn, test } from "bun:test";
import { deflateRaw, Inflate } from "pako";
import { Decoder } from "./decoder.ts";
import { Encoder } from "./encoder.ts";

const text = new TextEncoder();

test("group slices preserve a shared window across empty and nonempty frames", () => {
	const frames = ["the quick brown fox", "", "the quick brown dog", "the lazy fox"];
	const encoder = new Encoder();
	const decoder = new Decoder();
	for (const frame of frames.map((value) => text.encode(value))) {
		expect(decoder.frame(encoder.frame(frame), frame.length)).toEqual(frame);
	}
});

test("a retained frame owns only its payload, across later pushes", () => {
	const encoder = new Encoder();
	const decoder = new Decoder();
	const first = decoder.frame(encoder.frame(text.encode("first")), 5);
	decoder.frame(encoder.frame(new Uint8Array(20_000)), 20_000);
	expect(first).toEqual(text.encode("first"));
	expect(first.buffer.byteLength).toBe(first.length);
});

test("each declared size is exact, including an empty slice", () => {
	const slice = new Encoder().frame(new Uint8Array(1024));
	for (const size of [512, 2048]) expect(() => new Decoder().frame(slice, size)).toThrow(/declared size/);
	expect(() => new Decoder().frame(new Uint8Array(), 1)).toThrow(/declared size/);
});

test("a bomb stops inflating at the declaration plus one scratch chunk", () => {
	const slice = new Encoder().frame(new Uint8Array(1024 * 1024));
	const push = Inflate.prototype.push;
	let output = 0;
	const pushing = spyOn(Inflate.prototype, "push").mockImplementation(function (this: Inflate, ...args) {
		const onData = this.onData;
		this.onData = (chunk) => {
			output += chunk.length;
			onData.call(this, chunk);
		};
		try {
			return push.apply(this, args);
		} finally {
			this.onData = onData;
		}
	});
	try {
		expect(() => new Decoder().frame(slice, 1)).toThrow(/exceeds/);
		expect(output).toBe(4096);
	} finally {
		pushing.mockRestore();
	}
});

test("final blocks, malformed input, and a decoder already failed are refused", () => {
	const frame = text.encode("final");
	expect(() => new Decoder().frame(deflateRaw(frame), frame.length)).toThrow(/ended/);
	const decoder = new Decoder();
	expect(() => decoder.frame(new Uint8Array(64).fill(0xff), 64)).toThrow();
	expect(() => decoder.frame(new Uint8Array(), 0)).toThrow(/already failed/);
});

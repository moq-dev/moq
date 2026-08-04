import { expect, test } from "bun:test";
import { Decoder } from "./decoder.ts";
import { Encoder } from "./encoder.ts";

type Rec = { n: number };

function roundtrip(compression: boolean, values: Rec[]): Rec[] {
	const encoder = new Encoder<Rec>({ compression });
	const decoder = new Decoder<Rec>({ compression });
	return values.map((value) => decoder.decode(encoder.encode(value)));
}

test("plaintext roundtrip in order", () => {
	const values = Array.from({ length: 5 }, (_, n) => ({ n }));
	expect(roundtrip(false, values)).toEqual(values);
});

test("compressed roundtrip in order", () => {
	const values = Array.from({ length: 20 }, (_, n) => ({ n }));
	expect(roundtrip(true, values)).toEqual(values);
});

test("the shared window shrinks repetitive records", () => {
	const encoder = new Encoder<{ group: number; pts: number }>({ compression: true });
	const sizes = Array.from({ length: 8 }, (_, n) => encoder.encode({ group: n, pts: n * 2_000 }).length);

	const raw = new TextEncoder().encode(JSON.stringify({ group: 7, pts: 14_000 })).length;
	expect(sizes.at(-1)).toBeLessThan(raw / 2);
});

// A caller that rolls a group has to restart both windows, or the new group's frames decode against
// context the decoder on the other side never received.
test("reset starts a cold window on both sides", () => {
	const encoder = new Encoder<Rec>({ compression: true });
	const decoder = new Decoder<Rec>({ compression: true });

	for (let n = 0; n < 4; n += 1) {
		expect(decoder.decode(encoder.encode({ n }))).toEqual({ n });
	}

	encoder.reset();
	decoder.reset();

	expect(decoder.decode(encoder.encode({ n: 99 }))).toEqual({ n: 99 });
});

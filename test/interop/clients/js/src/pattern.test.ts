import { expect, test } from "bun:test";
import { CELLS, decode } from "./pattern";

// Frame five, generation two, with even parity. A literal pixel vector keeps
// the readback oracle independent of the painter it grades in real browsers.
const blocks = [1, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1];

function pixels(blocks: number[]): Uint8ClampedArray {
	const data = new Uint8ClampedArray(CELLS * 8 * 48 * 4);
	for (let y = 0; y < 48; y++) {
		for (let x = 0; x < CELLS * 8; x++) {
			data.fill(blocks[Math.floor(x / 8)] * 255, (y * CELLS * 8 + x) * 4, (y * CELLS * 8 + x) * 4 + 3);
		}
	}
	return data;
}

test("presented pixels name both the frame and its source generation", () => {
	expect(decode(pixels(blocks), CELLS * 8, 48)).toEqual({ frameId: 5, generation: 2, contrast: 255 });
});

test("a corrupted generation bit cannot become a valid new-generation picture", () => {
	const corrupted = [...blocks];
	corrupted[15] = 1;
	expect(decode(pixels(corrupted), CELLS * 8, 48)).toBeUndefined();
});

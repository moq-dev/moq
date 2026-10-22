import { expect, test } from "bun:test";
import { polyfill } from "./libav";

// Bun has no WebCodecs, so this runs on the polyfill's AudioData (another test file may have loaded it first),
// which libopus decodes into.
test("the polyfill deinterleaves every channel to f32-planar", async () => {
	expect(await polyfill()).toBe(true);

	const cases = [
		{ format: "f32", data: Float32Array.of(0.5, -0.5, 0.25, -0.25, 0, 1), scale: 1 },
		{ format: "s16", data: Int16Array.of(0x4000, -0x4000, 0x2000, -0x2000, 0, 0x7fff), scale: 0x8000 },
	] as const;
	for (const { format, data, scale } of cases) {
		const audio = new AudioData({
			format,
			sampleRate: 48_000,
			numberOfFrames: 3,
			numberOfChannels: 2,
			timestamp: 0,
			data,
		});
		for (const channel of [0, 1]) {
			const plane = new Float32Array(3);
			audio.copyTo(plane, { planeIndex: channel, format: "f32-planar" });
			expect(Array.from(plane)).toEqual([0, 1, 2].map((frame) => data[frame * 2 + channel] / scale));
		}

		const tail = new Float32Array(2);
		audio.copyTo(tail, { planeIndex: 1, format: "f32-planar", frameOffset: 1 });
		expect(Array.from(tail)).toEqual([1, 2].map((frame) => data[frame * 2 + 1] / scale));
		// A view smaller than the copy is refused, even with room in the buffer behind it.
		const backing = new Float32Array(8);
		expect(() =>
			audio.copyTo(backing.subarray(0, 1), { planeIndex: 1, format: "f32-planar", frameCount: 3 }),
		).toThrow(RangeError);
		expect(Array.from(backing)).toEqual(new Array(8).fill(0));
		// An unaligned byte view takes the floats too.
		const unaligned = new Uint8Array(13);
		audio.copyTo(unaligned.subarray(1), { planeIndex: 1, format: "f32-planar" });
		expect(Array.from(new Float32Array(unaligned.slice(1).buffer))).toEqual(
			[0, 1, 2].map((frame) => Math.fround(data[frame * 2 + 1] / scale)),
		);
		for (const bad of [
			{ planeIndex: -1 },
			{ planeIndex: 0.5 },
			{ planeIndex: 0, frameOffset: -1 },
			// WebCodecs refuses an offset at the end even though nothing would be copied.
			{ planeIndex: 0, frameOffset: 3 },
		]) {
			expect(() => audio.copyTo(backing, { ...bad, format: "f32-planar" })).toThrow(RangeError);
		}
		audio.close();
	}
});

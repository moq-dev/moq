import { expect, test } from "bun:test";
import { audioSpecificConfig } from "./aac";
import { polyfill } from "./libav";

const SAMPLE_RATE = 48000;

// 200ms of AAC-LC at 48kHz in ADTS: 440Hz left, 880Hz right, so a swapped or interleaved plane fails.
// Regenerate with:
// ffmpeg -f lavfi -i sine=f=440:r=48000:d=0.2 -f lavfi -i sine=f=880:r=48000:d=0.2 \
//   -filter_complex amerge -c:a aac -b:a 64k -f adts libav.test.aac
const FIXTURE = new URL("./libav.test.aac", import.meta.url);

// A description selects raw blocks; without one, each packet keeps its ADTS header.
function aacFrames(adts: Uint8Array, raw: boolean): Uint8Array[] {
	const frames: Uint8Array[] = [];
	for (let i = 0; i < adts.length; ) {
		const length = ((adts[i + 3] & 0x03) << 11) | (adts[i + 4] << 3) | (adts[i + 5] >> 5);
		const header = adts[i + 1] & 0x01 ? 7 : 9;
		frames.push(adts.subarray(i + (raw ? header : 0), i + length));
		i += length;
	}
	return frames;
}

// Goertzel power of one frequency.
function power(samples: Float32Array, frequency: number): number {
	const coeff = 2 * Math.cos((2 * Math.PI * frequency) / SAMPLE_RATE);
	let prev = 0;
	let prev2 = 0;
	for (const sample of samples) {
		const next = sample + coeff * prev - prev2;
		prev2 = prev;
		prev = next;
	}
	return prev * prev + prev2 * prev2 - coeff * prev * prev2;
}

test("the fallback decodes AAC without a native AudioDecoder", async () => {
	// The premise: Bun ships no WebCodecs, so this runs entirely on the libav fallback.
	expect(globalThis.AudioDecoder).toBeUndefined();
	expect(await polyfill()).toBe(true);

	const config: AudioDecoderConfig = {
		codec: "mp4a.40.2",
		sampleRate: SAMPLE_RATE,
		numberOfChannels: 2,
		description: audioSpecificConfig(SAMPLE_RATE, 2),
	};
	expect((await AudioDecoder.isConfigSupported(config)).supported).toBe(true);
	for (const codec of ["mp4a.40.02", "mp4a.40.5", "mp4a.40.05", "mp4a.40.29", "mp4a.67"]) {
		expect((await AudioDecoder.isConfigSupported({ ...config, codec })).supported).toBe(true);
	}

	for (const raw of [true, false]) {
		const planes: Float32Array[][] = [[], []];
		let failure: Error | undefined;
		const decoder = new AudioDecoder({
			output: (data) => {
				for (const [index, plane] of planes.entries()) {
					const samples = new Float32Array(data.numberOfFrames);
					data.copyTo(samples, { planeIndex: index, format: "f32-planar" });
					plane.push(samples);
				}
				data.close();
			},
			error: (error) => {
				failure = error;
			},
		});
		decoder.configure({ ...config, description: raw ? config.description : undefined });

		const frames = aacFrames(new Uint8Array(await Bun.file(FIXTURE).arrayBuffer()), raw);
		for (const [index, data] of frames.entries()) {
			decoder.decode(
				new EncodedAudioChunk({ type: "key", timestamp: (index * 1024 * 1_000_000) / SAMPLE_RATE, data }),
			);
		}
		await decoder.flush();
		decoder.close();
		expect(failure).toBeUndefined();

		const [left, right] = planes.map((chunks) => {
			const out = new Float32Array(chunks.reduce((sum, chunk) => sum + chunk.length, 0));
			let offset = 0;
			for (const chunk of chunks) {
				out.set(chunk, offset);
				offset += chunk.length;
			}
			return out;
		});
		expect(left.length).toBe(frames.length * 1024);
		expect(power(left, 440)).toBeGreaterThan(100 * power(left, 880));
		expect(power(right, 880)).toBeGreaterThan(100 * power(right, 440));
	}
});

// Runs on the polyfill's AudioData, which libopus decodes into. After the AAC test, which needs Bun's
// globals free of any WebCodecs before it installs the polyfill.
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
		audio.copyTo(tail, { planeIndex: 1, format: "f32-planar", frameOffset: 1, frameCount: 2 });
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

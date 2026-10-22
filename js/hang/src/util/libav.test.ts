import { expect, test } from "bun:test";
import { audioSpecificConfig } from "./aac";
import { polyfill } from "./libav";

const SAMPLE_RATE = 48000;

// 200ms of AAC-LC at 48kHz in ADTS: 440Hz left, 880Hz right, so a swapped or interleaved plane fails.
// Regenerate with:
// ffmpeg -f lavfi -i sine=f=440:r=48000:d=0.2 -f lavfi -i sine=f=880:r=48000:d=0.2 \
//   -filter_complex amerge -c:a aac -b:a 64k -f adts libav.test.aac
const FIXTURE = new URL("./libav.test.aac", import.meta.url);

// Strip the ADTS headers: a hang catalog carries the AudioSpecificConfig and frames are raw.
function rawFrames(adts: Uint8Array): Uint8Array[] {
	const frames: Uint8Array[] = [];
	for (let i = 0; i < adts.length; ) {
		const length = ((adts[i + 3] & 0x03) << 11) | (adts[i + 4] << 3) | (adts[i + 5] >> 5);
		const header = adts[i + 1] & 0x01 ? 7 : 9;
		frames.push(adts.subarray(i + header, i + length));
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
	decoder.configure(config);

	const frames = rawFrames(new Uint8Array(await Bun.file(FIXTURE).arrayBuffer()));
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
});

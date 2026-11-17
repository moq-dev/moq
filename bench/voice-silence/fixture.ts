// Writes the vad.test.ts signal as s16le, exactly as the port quantizes it, for the C harness.
// Usage: bun fixture.ts <rate> <out.s16>
import { writeFileSync } from "node:fs";

// Keep in sync with signal() in js/publish/src/audio/vad.test.ts.
function signal(rate: number): Float32Array {
	let state = 11;
	const random = () => {
		state = (Math.imul(state, 1103515245) + 12345) | 0;
		return (state >>> 8) / 0x1000000 - 0.5;
	};
	return Float32Array.from({ length: rate * 2 }, (_, n) => {
		const t = n / rate;
		const room = random() * 0.002;
		if (t >= 0.5 && t < 1) return room + random() * 0.6 * Math.sin(Math.PI * (t - 0.5) * 2);
		if (t >= 1 && t < 1.5) return room + 0.05 * Math.sin(2 * Math.PI * 300 * t) + 0.02 * Math.sin(2 * Math.PI * 1700 * t);
		// A quiet talker fading in, near the detector's threshold.
		if (t >= 1.5) return room + 0.006 * (t - 1.5) * random() * Math.sin(2 * Math.PI * 4 * t);
		return room;
	});
}

const rate = Number(process.argv[2]);
const samples = signal(rate);
const pcm = Int16Array.from(samples, (x) => Math.max(-32768, Math.min(32767, Math.round(x * 32768))));
writeFileSync(process.argv[3], new Uint8Array(pcm.buffer));

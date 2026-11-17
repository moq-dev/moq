// Prints, per 20 ms frame in [from, to) seconds of s16le 48 kHz input: its RMS, the VAD's Q8, and
// whether the gate passed it. Usage: bun trace.ts <file.s16> <from> <to>
import { readFileSync } from "node:fs";
import { Duration } from "@moq/clock";
import { Gate } from "../../js/publish/src/audio/gate";
import { Vad } from "../../js/publish/src/audio/vad";

const [file, from, to] = process.argv.slice(2);
const bytes = readFileSync(file);
const pcm = new Int16Array(bytes.buffer, bytes.byteOffset, bytes.byteLength >> 1);
const gate = new Gate({ sampleRate: 48_000, frame: Duration.millis(20) });
const scores: number[] = [];
const activity = Vad.prototype.activity;
Vad.prototype.activity = function (block: Int16Array) {
	const q8 = activity.call(this, block);
	scores.push(q8);
	return q8;
};
for (let i = 0; (i + 1) * 960 <= pcm.length; i++) {
	const samples = Float32Array.from(pcm.subarray(i * 960, (i + 1) * 960), (x) => x / 32768);
	const passed = gate.push({ timestamp: Duration.millis(i * 20), channels: [samples] }, Duration.millis(i * 20 + 20));
	const t = i * 0.02;
	if (t >= Number(from) && t < Number(to)) {
		const rms = 10 * Math.log10(samples.reduce((s, x) => s + x * x, 0) / samples.length + 1e-12);
		const sent = passed.map((p) => p.frame.timestamp.asMillis()).join(",");
		console.log(`${(t * 1000).toFixed(0)} ms rms ${rms.toFixed(1)} q8 ${scores[scores.length - 1]} open ${gate.open} sent [${sent}]`);
	}
}

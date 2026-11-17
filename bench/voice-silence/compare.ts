// Prints the JS port's speech_activity_Q8 per block for s16le mono PCM, fed in 128-sample quanta.
// Usage: bun compare.ts <file.s16> <rate> <block ms>
import { readFileSync } from "node:fs";
import { Duration } from "@moq/clock";
import { Vad } from "../../js/publish/src/audio/vad";

const [file, rate, block] = process.argv.slice(2);
const bytes = readFileSync(file);
const pcm = new Int16Array(bytes.buffer, bytes.byteOffset, bytes.byteLength >> 1);
const vad = new Vad({ sampleRate: Number(rate), block: Number(block) as 10 | 20 });

const scores: number[] = [];
const activity = vad.activity.bind(vad);
vad.activity = (block: Int16Array) => {
	const q8 = activity(block);
	scores.push(q8);
	return q8;
};

const quantum = 128;
for (let at = 0; at + quantum <= pcm.length; at += quantum) {
	const chunk = new Float32Array(quantum);
	for (let i = 0; i < quantum; i++) chunk[i] = pcm[at + i] / 32768;
	vad.push({ timestamp: Duration.micros((at / Number(rate)) * 1_000_000), channels: [chunk] });
}
console.log(scores.join("\n"));

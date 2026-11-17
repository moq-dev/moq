// Runs the Gate over s16le mono 48 kHz PCM in 20 ms frames and prints, per segment of the signal,
// how many frames it passed. Usage: bun gate.ts <file.s16> [segments as name:start:end,...]
import { readFileSync } from "node:fs";
import { Duration } from "@moq/clock";
import { Gate } from "../../js/publish/src/audio/gate";

const [file, spec] = process.argv.slice(2);
const bytes = readFileSync(file);
const pcm = new Int16Array(bytes.buffer, bytes.byteOffset, bytes.byteLength >> 1);
const gate = new Gate({ sampleRate: 48_000, frame: Duration.millis(20) });

const passed = new Set<number>();
let closes = 0;
for (let i = 0; (i + 1) * 960 <= pcm.length; i++) {
	const samples = Float32Array.from(pcm.subarray(i * 960, (i + 1) * 960), (x) => x / 32768);
	const open = gate.open;
	for (const out of gate.push({ timestamp: Duration.millis(i * 20), channels: [samples] }, Duration.millis(i * 20 + 20))) {
		passed.add(Math.round(out.frame.timestamp.asMillis() / 20));
	}
	if (open && !gate.open) closes++;
}
const total = Math.floor(pcm.length / 960);
const segments = (spec ?? `all:0:${total / 50}`).split(",").map((s) => s.split(":"));
for (const [name, start, end] of segments) {
	let n = 0;
	for (let i = Number(start) * 50; i < Number(end) * 50; i++) if (passed.has(i)) n++;
	console.log(`${name} ${start}-${end} s: ${n} of ${(Number(end) - Number(start)) * 50} frames sent`);
}
console.log(`total: ${passed.size} of ${total} frames sent, ${closes} pauses`);

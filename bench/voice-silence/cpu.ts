// CPU per minute of 48 kHz audio for the analysis and the gate, after two warm-up passes.
// Usage: bun cpu.ts <file.s16>
import { Duration } from "@moq/clock";
import { Analysis } from "../../js/publish/src/audio/analysis";
import { Gate } from "../../js/publish/src/audio/gate";

const pcm = new Int16Array(await Bun.file(process.argv[2]).arrayBuffer());
const frames: Float32Array[] = [];
for (let at = 0; at + 960 <= pcm.length; at += 960)
	frames.push(Float32Array.from(pcm.subarray(at, at + 960), (x) => x / 32768));
const time = (name: string, make: () => (frame: Float32Array, i: number) => void) => {
	for (let pass = 0; pass < 3; pass++) {
		const run = make();
		const start = performance.now();
		for (const [i, frame] of frames.entries()) run(frame, i);
		if (pass === 2) console.log(`${name}: ${(performance.now() - start).toFixed(1)} ms per ${frames.length / 50} s`);
	}
};
time("analysis", () => {
	const analysis = new Analysis({ sampleRate: 48_000 });
	return (frame, i) => analysis.push({ timestamp: Duration.millis(i * 20), channels: [frame] });
});
time("gate", () => {
	const gate = new Gate({ sampleRate: 48_000, frame: Duration.millis(20) });
	return (frame, i) => gate.push({ timestamp: Duration.millis(i * 20), channels: [frame] }, Duration.millis(i * 20 + 20));
});

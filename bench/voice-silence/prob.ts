// Prints the port's per-frame activity probability for s16le mono PCM, as ref_analysis does.
// Usage: bun prob.ts <file.s16> <rate> <frame samples>
import { Duration } from "@moq/clock";
import { Analysis } from "../../js/publish/src/audio/analysis";

const [file, rateArg, frameArg] = process.argv.slice(2);
const rate = Number(rateArg);
const frame = Number(frameArg);
const bytes = await Bun.file(file).arrayBuffer();
const pcm = new Int16Array(bytes);
const analysis = new Analysis({ sampleRate: rate });
const out: string[] = [];
const start = performance.now();
for (let at = 0; at + frame <= pcm.length; at += frame) {
	const samples = Float32Array.from(pcm.subarray(at, at + frame), (x) => x / 32768);
	const p = analysis.probability({ timestamp: Duration.micros((at / rate) * 1e6), channels: [samples] });
	out.push(p === undefined ? "invalid" : p.toPrecision(9));
}
console.error(`${(performance.now() - start).toFixed(0)} ms for ${(pcm.length / rate).toFixed(1)} s`);
console.log(out.join("\n"));

// Prints, per frame of s16le mono PCM, 1 when the port's activity would have libopus in DTX (over
// 200 ms inactive). Above 16 kHz each 20 ms piece of a longer frame is decided, as libopus's hybrid
// and CELT modes do, and the frame is shut only when every piece is.
// Usage: bun decide.ts <file.s16> <rate> <frame samples>
import { Duration } from "@moq/clock";
import { Analysis } from "../../js/publish/src/audio/analysis";

const [file, rateArg, frameArg] = process.argv.slice(2);
const rate = Number(rateArg);
const frame = Number(frameArg);
const piece = rate > 16_000 ? Math.min(frame, rate / 50) : frame;
const pcm = new Int16Array(await Bun.file(file).arrayBuffer());
const analysis = new Analysis({ sampleRate: rate });
const out: number[] = [];
let quiet = 0;
for (let at = 0; at + frame <= pcm.length; at += frame) {
	let shut = true;
	for (let p = at; p < at + frame; p += piece) {
		const samples = Float32Array.from(pcm.subarray(p, p + piece), (x) => x / 32768);
		const active = analysis.push({ timestamp: Duration.micros((p / rate) * 1e6), channels: [samples] });
		quiet = active ? 0 : quiet + (piece * 1000) / rate;
		shut &&= quiet > 200;
	}
	out.push(shut ? 1 : 0);
}
console.log(out.join("\n"));

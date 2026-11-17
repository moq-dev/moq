// Compares the frames the Gate sends with the frames libopus sends (dtx's output), allowing the
// gate's pre-roll before each reopening.
// Usage: bun gatevs.ts <file.s16> <rate> <frame samples> <dtx.txt>
import { Duration } from "@moq/clock";
import { Gate } from "../../js/publish/src/audio/gate";

const [file, rateArg, frameArg, dtxFile] = process.argv.slice(2);
const rate = Number(rateArg);
const frame = Number(frameArg);
const us = (frame * 1e6) / rate;
const pcm = new Int16Array(await Bun.file(file).arrayBuffer());
const dtx = (await Bun.file(dtxFile).text()).trim().split("\n").map(Number);
const gate = new Gate({ sampleRate: rate, frame: Duration.micros(us) });
const sent = new Set<number>();
for (let i = 0; (i + 1) * frame <= pcm.length; i++) {
	const samples = Float32Array.from(pcm.subarray(i * frame, (i + 1) * frame), (x) => x / 32768);
	for (const out of gate.push({ timestamp: Duration.micros(i * us), channels: [samples] }, Duration.micros((i + 1) * us)))
		sent.add(Math.round(out.frame.timestamp.asMicros() / us));
}
const preroll = Math.max(1, Math.round(20_000 / us));
let extra = 0;
let missing = 0;
for (let i = 0; i < dtx.length; i++) {
	const libopus = dtx[i] === 0;
	// A frame libopus sends within the pre-roll before a reopening is expected either way.
	const beforeOpen = !libopus && dtx.slice(i + 1, i + 1 + preroll).some((d) => d === 0);
	if (sent.has(i) && !libopus && !beforeOpen) extra++;
	if (!sent.has(i) && libopus) missing++;
}
console.log(`frames ${dtx.length}: gate sent ${sent.size}, libopus ${dtx.filter((d) => d === 0).length}; gate-only ${extra}, libopus-only ${missing}`);

/** The page `ring.ts` loads: N players feeding the real render worklet through `AudioBuffer`. */
import { Micro } from "@moq/clock";
import { AudioBuffer } from "../src/audio/buffer";

const RATE = 48_000;
const FRAME = 960; // one 20 ms Opus frame per callback

const count = Number(new URL(location.href).searchParams.get("players"));
const context = new AudioContext({ sampleRate: RATE, latencyHint: "interactive" });
await context.audioWorklet.addModule("/render-worklet.js");
const players = Array.from({ length: count }, () => {
	const worklet = new AudioWorkletNode(context, "render", { outputChannelCount: [2] });
	worklet.connect(context.destination);
	// The 200 ms floor, unbuffered: what a live viewer plays at.
	const buffer = new AudioBuffer(worklet, { channels: 2, rate: RATE, latency: RATE / 5, buffered: false });
	return { worklet, buffer };
});
await context.resume();

const tone = new Float32Array(FRAME);
for (let i = 0; i < FRAME; i++) tone[i] = Math.sin((i / RATE) * 440 * 2 * Math.PI) * 0.01;

let callbacks = 0;
let running = true;
const start = context.currentTime;
const tick = () => {
	if (!running) return;
	// Paced from the device clock: relative timers alone slowly underfeed a real worklet.
	while ((callbacks * FRAME) / RATE <= context.currentTime - start) {
		// A decoder hands over fresh planes every frame, which the ring then transfers.
		for (const { buffer } of players)
			buffer.insert(Micro((callbacks * FRAME * 1_000_000) / RATE), [tone.slice(), tone.slice()]);
		callbacks++;
	}
	setTimeout(tick, FRAME / (RATE / 1000));
};
tick();

Object.assign(globalThis, {
	ring: {
		stats: () => ({
			callbacks,
			context: context.state,
			players: players.map(({ buffer }) => ({
				timestamp: buffer.timestamp.peek(),
				stalled: buffer.stalled.peek(),
				underruns: buffer.underruns.peek(),
			})),
		}),
		stop: async () => {
			running = false;
			for (const { buffer, worklet } of players) {
				buffer.close();
				worklet.disconnect();
			}
			await context.close();
		},
	},
});

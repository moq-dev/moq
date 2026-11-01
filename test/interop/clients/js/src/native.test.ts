import { expect, test } from "bun:test";
import * as Native from "./native";
import * as Pattern from "./pattern";

// The oracle is the browser's own decoder: ffmpeg renders the filters and decode must name every
// frame, so the native picture cannot drift from the one the fixture paints.
const ffmpeg = Bun.which("ffmpeg");

function render(args: string[]): Uint8Array {
	const run = Bun.spawnSync([ffmpeg ?? "ffmpeg", "-hide_banner", "-loglevel", "error", ...args]);
	if (run.exitCode !== 0) throw new Error(`ffmpeg failed: ${run.stderr.toString()}`);
	return run.stdout;
}

test.skipIf(!ffmpeg)("the native picture decodes as the frame ffmpeg numbered it", () => {
	// The first frames, every counter bit flipping into its high state, and the frames around the wrap.
	const ranges = [
		[0, 40],
		[4090, 4100],
		[8185, 8195],
	];
	const size = Pattern.WIDTH * Pattern.HEIGHT * 4;
	for (const [from, to] of ranges) {
		const raw = render([
			"-f",
			"lavfi",
			"-i",
			`${Native.video(2)},trim=start_frame=${from}:end_frame=${to}`,
			"-pix_fmt",
			"rgba",
			"-f",
			"rawvideo",
			"-",
		]);
		const wanted = Array.from({ length: to - from }, (_, i) => from + i);
		expect(raw.length).toBe(size * wanted.length);
		const decoded = wanted.map((_, i) =>
			Pattern.decode(
				new Uint8ClampedArray(raw.buffer, raw.byteOffset + i * size, size),
				Pattern.WIDTH,
				Pattern.HEIGHT,
			),
		);
		expect(decoded.map((d) => d?.generation)).toEqual(wanted.map(() => 2));
		expect(decoded.map((d) => d?.frameId)).toEqual(wanted.map((n) => n % 2 ** Pattern.FRAME_BITS));
	}
});

test.skipIf(!ffmpeg)("the native tone plays each step's frequency for its whole step", () => {
	const rate = 48_000;
	const seconds = (Pattern.STEPS * Pattern.STEP_MS) / 1000;
	const raw = render(["-f", "lavfi", "-i", `${Native.audio(rate)}:d=${seconds}`, "-f", "f32le", "-"]);
	const samples = new Float32Array(raw.buffer, raw.byteOffset, raw.byteLength / 4);
	const perStep = (rate * Pattern.STEP_MS) / 1000;

	for (let step = 0; step < Pattern.STEPS; step++) {
		const window = samples.subarray(step * perStep, (step + 1) * perStep);
		// The strongest frequency on the table, by correlating against each step's sine and cosine.
		let best = -1;
		let bestPower = 0;
		for (let candidate = 0; candidate < Pattern.STEPS; candidate++) {
			const w = (2 * Math.PI * Pattern.stepFrequency(candidate)) / rate;
			let re = 0;
			let im = 0;
			for (let i = 0; i < window.length; i++) {
				re += window[i] * Math.cos(w * i);
				im += window[i] * Math.sin(w * i);
			}
			const power = re * re + im * im;
			if (power > bestPower) {
				bestPower = power;
				best = candidate;
			}
		}
		expect(best).toBe(step);
	}

	// Phase-continuous at every boundary: no sample jumps further than the highest step can move.
	const limit = 2 * Math.PI * Pattern.stepFrequency(Pattern.STEPS - 1) * (0.5 / rate) * 1.01;
	let jump = 0;
	for (let i = 1; i < samples.length; i++) jump = Math.max(jump, Math.abs(samples[i] - samples[i - 1]));
	expect(jump).toBeLessThan(limit);
});

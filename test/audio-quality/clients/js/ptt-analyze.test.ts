import { expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import type { Sample } from "./src/schema";

const FRAME = 20;
/** The playout delay `"auto"` holds through the run. */
const DELAY = 60;
/** How far the native clock runs ahead of the page's, delivery included. */
const NATIVE = 5000;

// 22 complete presses on an ideal path: the media clock is the page clock, every 20 ms frame is
// encoded, written, received and decoded a millisecond apart, and each separated press renders
// once the 60 ms delay is decoded, with 100 ms of projected device output after it.
function trace() {
	const result = {
		submitted: [] as { at: number; timestamp: number; duration: number }[],
		encoded: [] as { at: number; timestamp: number; bytes: number }[],
		sent: [] as { at: number; timestamp: number; bytes: number }[],
		decoding: [] as { at: number; timestamp: number }[],
		decoded: [] as { at: number; timestamp: number; duration: number }[],
		subscriptions: [] as unknown[],
		audible: [] as { at: number; rendered: number; received: number }[],
		presses: [] as { at: number; on: boolean; capture?: { at: number; end: number } }[],
		captures: [] as unknown[],
		encoders: [] as number[],
		catalog: [] as { at: number; selectable: boolean }[],
		playback: { underruns: 0, stalls: 0 },
		errors: [] as string[],
		arrivals: [] as [number, number, number, number][],
		delays: [{ at: 0, delay: DELAY }],
	};
	for (let i = 0; i < 22; i++) {
		const press = 10_000 + i * 10_000;
		const release = press + 2000;
		// The press lands 10 ms into a frame, so the head frame still needs 10 ms of media.
		result.presses.push({ at: press, on: true, capture: { at: press, end: press * 1000 } });
		const start = press - 10;
		for (let media = start; media + FRAME <= release; media += FRAME) {
			const at = media + FRAME + 25;
			if (at >= release) break;
			const timestamp = media * 1000;
			result.submitted.push({ at, timestamp, duration: FRAME * 1000 });
			result.encoded.push({ at: at + 1, timestamp, bytes: 959 });
			result.sent.push({ at: at + 1, timestamp, bytes: 959 });
			result.arrivals.push([at + 2, media, i, FRAME]);
			result.decoding.push({ at: at + 2, timestamp });
			result.decoded.push({ at: at + 3, timestamp, duration: FRAME * 1000 });
		}
		if (i === 0 || i >= 8) {
			// The third frame completes the delay; the render onset follows its PCM by 10 ms.
			const rendered = start + DELAY + 25 + 3 + 10;
			result.audible.push({ at: rendered + 100, rendered, received: rendered + 5 });
		}
		result.presses.push({ at: release, on: false });
	}
	return result;
}

// The native listener on the same run: it resumes on each head 2 ms after the page writes it,
// plays each previous utterance out 300 ms after its release, and fills the rest of the mute with
// comfort noise. Samples every 250 ms, with the render clock half a second behind.
function nativeTrace(result: ReturnType<typeof trace>) {
	const presses = result.presses.filter((press) => press.on);
	const mutes = presses.slice(1).map((press, i) => [result.presses[i * 2 + 1].at + 300, press.at + 300]);
	const heads = presses.map((press) => {
		const head = result.submitted.find((frame) => frame.at >= press.at);
		const written = result.sent.find((frame) => frame.timestamp === head?.timestamp);
		if (!head || !written) throw new Error("no head");
		return { at: written.at + 2 + NATIVE, timestamp: head.timestamp / 1000 };
	});
	const samples: Sample[] = [];
	const end = result.presses[result.presses.length - 1].at + 5000;
	for (let page = 5000; page <= end; page += 250) {
		const at = page + NATIVE;
		samples.push({
			at,
			render: at - 500,
			stalled: false,
			comfort: mutes.reduce((sum, [from, to]) => sum + Math.min(Math.max(page - from, 0), to - from), 0),
			utterances: heads.filter((head) => head.at <= at && head.at > at - 250),
		});
	}
	return samples;
}

async function analyze(result: ReturnType<typeof trace>, native = nativeTrace(result)) {
	const directory = mkdtempSync(join(tmpdir(), "ptt-analyze-"));
	try {
		await Bun.write(join(directory, "presses.json"), JSON.stringify(result));
		await Bun.write(join(directory, "native.ndjson"), native.map((sample) => JSON.stringify(sample)).join("\n"));
		const run = Bun.spawnSync(["bun", fileURLToPath(new URL("./ptt-analyze.ts", import.meta.url)), directory]);
		const summary = await Bun.file(join(directory, "summary.json")).json();
		return { exitCode: run.exitCode, stdout: run.stdout.toString(), stderr: run.stderr.toString(), summary };
	} finally {
		rmSync(directory, { recursive: true, force: true });
	}
}

test("a complete run passes, reports its latency, and splits each press exactly", async () => {
	const { exitCode, stdout, stderr, summary } = await analyze(trace());
	expect(stderr).toBe("");
	expect(exitCode).toBe(0);
	expect(JSON.parse(stdout).latency.playout).not.toBeNull();

	const press = summary.presses[0];
	expect(press.path).toEqual({
		capture: 25,
		packetization: 10,
		encode: 1,
		network: 1,
		decode: 1,
		jitter_buffer: DELAY - FRAME,
		render: 10,
		device: 100,
	});
	const parts = Object.values(press.path as Record<string, number>).reduce((sum, part) => sum + part, 0);
	expect(parts).toBe(press.audible);
	expect(summary.delay).toEqual({ min: DELAY, median: DELAY, max: DELAY });
});

test("a target that rises before the delay fills is filled by the frame that reaches it", async () => {
	// "auto" raises the target a frame as the press starts, so the fourth frame fills it.
	const result = trace();
	result.delays.push({ at: result.presses[0].at, delay: DELAY + FRAME });
	result.audible[0].rendered += FRAME;
	result.audible[0].at += FRAME;
	const { exitCode, summary } = await analyze(result);
	expect(exitCode).toBe(0);
	expect(summary.presses[0].delay).toBe(DELAY + FRAME);
	expect(summary.presses[0].path.jitter_buffer).toBe(DELAY);
	expect(summary.presses[0].path.render).toBe(10);
});

test("a frame held back by a capture burst widens its press's spread", async () => {
	const result = trace();
	const press = result.presses[4].at;
	const held = result.arrivals.find((arrival) => arrival[0] > press + 3 * FRAME);
	if (!held) throw new Error("no frame to hold");
	held[0] += 30;
	const { exitCode, summary } = await analyze(result);
	expect(exitCode).toBe(0);
	expect(summary.presses[2].spread).toBeCloseTo(30);
	expect(summary.latency.spread).toEqual({ min: 0, median: 0, max: expect.closeTo(30) });
});

test("the delay is filled by the first frame to reach it, not the newest before the onset", async () => {
	// The fourth frame decodes just before the onset, adding to a ring already leaving its stall.
	const result = trace();
	const fourth = (result.presses[0].at - 10 + DELAY) * 1000;
	const late = result.decoded.find((frame) => frame.timestamp === fourth);
	if (!late) throw new Error("no fourth frame");
	late.at = result.audible[0].rendered - 1;
	const { exitCode, summary } = await analyze(result);
	expect(exitCode).toBe(0);
	expect(summary.presses[0].path.jitter_buffer).toBe(DELAY - FRAME);
	expect(summary.presses[0].path.render).toBe(10);
});

test("a head that never reaches the listener fails the run", async () => {
	const result = trace();
	const head = result.submitted.find((frame) => frame.at >= result.presses[2].at);
	result.arrivals = result.arrivals.filter((arrival) => arrival[1] * 1000 !== head?.timestamp);
	const { exitCode, stderr } = await analyze(result);
	expect(exitCode).toBe(1);
	expect(stderr).toContain("utterance head was not received");
});

test("a head that arrives but never reaches the decoder fails the run", async () => {
	// A head reordered behind the next frame's group, then dropped before decode.
	const result = trace();
	const head = result.submitted.find((frame) => frame.at >= result.presses[2].at);
	result.decoding = result.decoding.filter((frame) => frame.timestamp !== head?.timestamp);
	result.decoded = result.decoded.filter((frame) => frame.timestamp !== head?.timestamp);
	const { exitCode, stderr } = await analyze(result);
	expect(exitCode).toBe(1);
	expect(stderr).toContain("utterance head was not decoded");
});

test("a new encoder fails the run", async () => {
	const result = trace();
	result.encoders.push(result.presses[2].at + 1);
	const { exitCode, stderr } = await analyze(result);
	expect(exitCode).toBe(1);
	expect(stderr).toContain("an encoder was created again");
});

test("a mute that disables the rendition fails the run", async () => {
	const result = trace();
	result.catalog.push({ at: result.presses[3].at, selectable: false });
	const { exitCode, stderr } = await analyze(result);
	expect(exitCode).toBe(1);
	expect(stderr).toContain("a mute disabled or dropped the rendition");
});

// The silence between presses is declared, so playback crosses it without counting it. Underruns
// and stalls the player does count are reported, not failed: zero underruns is not a goal.
test("an underrun or stall across the presses is reported and passes", async () => {
	const result = trace();
	result.playback = { underruns: 3, stalls: 2 };
	const { exitCode, summary } = await analyze(result);
	expect(exitCode).toBe(0);
	expect(summary.playback).toEqual({ underruns: 3, stalls: 2 });
});

test("the native listener's results sit beside each press", async () => {
	const { exitCode, summary } = await analyze(trace());
	expect(exitCode).toBe(0);
	expect(summary.native.offset).toBe(NATIVE + 2);
	expect(summary.native.playback).toEqual({ underruns: 0, stalls: 0 });
	expect(summary.presses[0].native).toEqual({ head: true, comfort: null });
	// A separated press's mute, less the 300 ms its previous utterance still played, give or take a
	// sample on each side.
	const comfort = summary.presses[8].native.comfort;
	expect(Math.abs(comfort - (8000 - 300))).toBeLessThanOrEqual(250);
});

test("a native utterance resumed past its head fails the run", async () => {
	// The head was lost, so the next frame started the utterance.
	const result = trace();
	const native = nativeTrace(result);
	const resumed = native.flatMap((sample) => sample.utterances ?? [])[4];
	resumed.timestamp += FRAME;
	const { exitCode, stderr } = await analyze(result, native);
	expect(exitCode).toBe(1);
	expect(stderr).toContain("native: an utterance did not resume on its head");
});

test("a native mute without comfort noise fails the run", async () => {
	const result = trace();
	const native = nativeTrace(result);
	const from = result.presses[19].at + NATIVE;
	let comfort: number | undefined;
	for (const sample of native) {
		if (sample.at < from) continue;
		comfort ??= sample.comfort;
		if (sample.at < result.presses[20].at + NATIVE + 300) sample.comfort = comfort;
	}
	const { exitCode, stderr } = await analyze(result, native);
	expect(exitCode).toBe(1);
	expect(stderr).toContain("native: a mute played no comfort noise");
});

test("a native underrun or stall counts from the first press on, and passes", async () => {
	for (const [page, counted] of [
		[trace().presses[0].at - 1000, 0],
		[trace().presses[6].at, 1],
	] as const) {
		for (const event of ["gap", "stall"] as const) {
			const run = trace();
			const native = nativeTrace(run);
			const sample = native.find((sample) => sample.at >= page + NATIVE + 250);
			if (!sample?.render) throw new Error("no sample");
			// Reported a sample late, stamped where it played.
			const at = sample.render - 250;
			if (event === "gap") sample.gaps = [{ at, ms: 20, quanta: 8, short: 1 }];
			else sample.stalls = [{ at, stalled: true }];
			const { exitCode, summary } = await analyze(run, native);
			expect(exitCode).toBe(0);
			expect(summary.native.playback).toEqual(
				event === "gap" ? { underruns: counted, stalls: 0 } : { underruns: 0, stalls: counted },
			);
		}
	}
});

test("a native underrun open as the first press lands counts", async () => {
	const result = trace();
	const native = nativeTrace(result);
	const sample = native.find((sample) => sample.at >= result.presses[0].at + NATIVE + 250);
	if (!sample?.render) throw new Error("no sample");
	// It starts 100 ms before the window, which opens at the first press's head, and runs 200 ms into it.
	const start = result.presses[0].at + NATIVE + 2;
	sample.gaps = [{ at: sample.render - (sample.at - (start - 100)), ms: 300, quanta: 113, short: 0 }];
	const { exitCode, summary } = await analyze(result, native);
	expect(exitCode).toBe(0);
	expect(summary.native.playback.underruns).toBe(1);
});

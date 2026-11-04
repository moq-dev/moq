import { expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const FRAME = 60;

// 22 complete presses on an ideal path: the media clock is the page clock, every 60 ms frame is
// encoded, written, received and decoded a millisecond apart, and each separated press renders
// once the 300 ms delay is decoded, with 100 ms of projected device output after it.
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
		errors: [] as string[],
		arrivals: [] as [number, number, number, number][],
		state: { delay: 300 },
	};
	for (let i = 0; i < 22; i++) {
		const press = 10_000 + i * 10_000;
		const release = press + 2000;
		// The press lands 20 ms into a frame, so the head frame still needs 40 ms of media.
		result.presses.push({ at: press, on: true, capture: { at: press, end: press * 1000 } });
		const start = press - 20;
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
			// The fifth frame completes the delay; the render onset follows its PCM by 10 ms.
			const rendered = start + 5 * FRAME + 25 + 3 + 10;
			result.audible.push({ at: rendered + 100, rendered, received: rendered + 5 });
		}
		result.presses.push({ at: release, on: false });
	}
	return result;
}

async function analyze(result: ReturnType<typeof trace>) {
	const directory = mkdtempSync(join(tmpdir(), "ptt-analyze-"));
	try {
		await Bun.write(join(directory, "presses.json"), JSON.stringify(result));
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
		packetization: 40,
		encode: 1,
		network: 1,
		decode: 1,
		jitter_buffer: 4 * FRAME,
		render: 10,
		device: 100,
	});
	const parts = Object.values(press.path as Record<string, number>).reduce((sum, part) => sum + part, 0);
	expect(parts).toBe(press.audible);
});

test("a new encoder fails as a probe, but the live encoder's release flush does not", async () => {
	// A flush leaves no trace in the page's records, so the passing run above is the release flush.
	const result = trace();
	result.encoders.push(result.presses[2].at + 1);
	const { exitCode, stderr } = await analyze(result);
	expect(exitCode).toBe(1);
	expect(stderr).toContain("an encoder was created again");
});

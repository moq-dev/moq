import { expect, mock, test } from "bun:test";
import type { FromWorker, ToWorker } from "./capture-worker.ts";

// Whether the fake worker claims a native MediaStreamTrackProcessor, how much its source clock
// advances per frame (WebKit's canvas tracks don't advance at all), and every worker spawned so far.
let supported = true;
let advance = 1000;
// Added to each frame's arrival time, so a test can space arrivals apart by more than the clock
// resolution rather than relying on performance.now() ticking between microtasks.
let arrivalStep = 0;
const spawned: FakeWorker[] = [];

// Stands in for the real capture worker: reports support on load, then answers each pull with a
// frame whose timestamp advances by 1000us.
class FakeWorker {
	onmessage: ((event: MessageEvent<FromWorker>) => void) | null = null;
	onerror: unknown = null;
	onmessageerror: unknown = null;

	terminated = false;
	started?: FakeTrack;

	#timestamp = 5_000_000; // the camera's own epoch, which the rewrite has to erase
	#frames = 0;
	#at = performance.now(); // frozen, so arrivals only move by arrivalStep

	constructor() {
		spawned.push(this);
		this.#emit({ type: "ready", supported });
	}

	postMessage(msg: ToWorker): void {
		if (msg.type === "start") {
			this.started = msg.track as unknown as FakeTrack;
			return;
		}

		this.#emit({
			type: "frame",
			frame: new FakeVideoFrame(this.#timestamp) as unknown as VideoFrame,
			at: performance.timeOrigin + this.#at + this.#frames * arrivalStep,
		});
		this.#timestamp += advance;
		this.#frames += 1;
	}

	terminate(): void {
		this.terminated = true;
	}

	#emit(msg: FromWorker): void {
		queueMicrotask(() => this.onmessage?.({ data: msg } as MessageEvent<FromWorker>));
	}
}

// Doubles as the global VideoFrame, whose (frame, init) overload is how the rewrite stage restamps
// a frame without copying its pixels.
class FakeVideoFrame {
	readonly timestamp: number;
	closed = false;

	constructor(source: number | FakeVideoFrame, init?: { timestamp: number }) {
		this.timestamp = init ? init.timestamp : (source as number);
	}

	close(): void {
		this.closed = true;
	}
}

class FakeTrack {
	stopped = false;
	clones = 0;

	clone(): FakeTrack {
		this.clones += 1;
		return new FakeTrack();
	}

	stop(): void {
		this.stopped = true;
	}
}

// The capture worker is imported as an inlined blob URL, which the bun test loader can't resolve.
mock.module("./capture-worker.ts?worker&inline", () => ({ default: FakeWorker }));

Object.defineProperty(globalThis, "VideoFrame", { configurable: true, writable: true, value: FakeVideoFrame });

const { TrackProcessor } = await import("./processor.ts");

test("captures through the worker, transferring a clone", async () => {
	supported = true;
	advance = 1000;
	arrivalStep = 0;
	spawned.length = 0;

	// The worker is constructed synchronously inside TrackProcessor, so bracket the whole call.
	const before = performance.now() * 1000;

	const track = new FakeTrack();
	const stream = TrackProcessor(track as unknown as Parameters<typeof TrackProcessor>[0]);
	const reader = stream.getReader();

	const first = await reader.read();
	const second = await reader.read();
	const after = performance.now() * 1000;

	expect(spawned).toHaveLength(1);
	const worker = spawned[0];

	// The caller keeps its own track (for the preview, settings and stop), so the worker gets a clone.
	expect(track.clones).toBe(1);
	expect(track.stopped).toBe(false);
	expect(worker.started).toBeDefined();
	expect(worker.started).not.toBe(track);

	// The camera's epoch is dropped: the first frame lands on the wall clock reading the worker took
	// when it read the frame, which is bounded by our own two readings.
	expect(first.value?.timestamp).toBeGreaterThanOrEqual(before);
	expect(first.value?.timestamp).toBeLessThanOrEqual(after);

	// Everything after the first keeps its original spacing.
	expect((second.value?.timestamp ?? 0) - (first.value?.timestamp ?? 0)).toBe(1000);

	await reader.cancel();
	expect(worker.terminated).toBe(true);
});

test("falls back when the worker has no MediaStreamTrackProcessor", async () => {
	supported = false;
	advance = 1000;
	arrivalStep = 0;
	spawned.length = 0;

	const track = new FakeTrack();
	const stream = TrackProcessor(track as unknown as Parameters<typeof TrackProcessor>[0]);
	const reader = stream.getReader();

	// The fallback needs a <video> element, which bun doesn't have: it fails rather than hanging,
	// which is enough to show we didn't take the worker path.
	await expect(reader.read()).rejects.toThrow();

	expect(spawned).toHaveLength(1);
	expect(spawned[0].terminated).toBe(true);

	// Nothing was handed over, so the caller's track is untouched.
	expect(track.clones).toBe(0);
	expect(track.stopped).toBe(false);
});

test("falls back to arrival time when the source clock is stuck", async () => {
	supported = true;
	// WebKit reports 0 for every frame off a canvas capture track, which the file source uses there.
	advance = 0;
	arrivalStep = 10;
	spawned.length = 0;

	const track = new FakeTrack();
	const stream = TrackProcessor(track as unknown as Parameters<typeof TrackProcessor>[0]);
	const reader = stream.getReader();

	const first = await reader.read();
	const second = await reader.read();
	const third = await reader.read();

	// Without the fallback all three would land on the same instant, and the encoder would see a
	// stream of frames that never advances. Arrivals are 10ms apart, so the deltas are exact.
	expect((second.value?.timestamp ?? 0) - (first.value?.timestamp ?? 0)).toBe(10_000);
	expect((third.value?.timestamp ?? 0) - (second.value?.timestamp ?? 0)).toBe(10_000);

	await reader.cancel();
});

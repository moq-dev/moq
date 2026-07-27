import { Time } from "@moq/net";
import type { FromWorker, ToWorker } from "./capture-worker";
// Compiled and inlined as a blob URL by Vite.
import CaptureWorker from "./capture-worker.ts?worker&inline";
import type { StreamTrack } from "./types";

/**
 * Turns a camera track into a stream of frames, using the best pipeline the engine has.
 *
 * Timestamps are rewritten onto our wall clock so audio and video share one epoch, and so they stay
 * consistent when the source changes or the encoder reloads.
 */
export function TrackProcessor(track: StreamTrack): ReadableStream<VideoFrame> {
	// Chrome exposes MediaStreamTrackProcessor on the window, so use it directly.
	if ("MediaStreamTrackProcessor" in self) {
		const input: ReadableStream<VideoFrame> = new MediaStreamTrackProcessor({ track }).readable;
		return input.pipeThrough(rewrite());
	}

	// Safari and Firefox expose it only inside a dedicated worker, which means spawning one to find
	// out. Defer the choice to the first pull instead of making every caller await it.
	return deferred(async () => (await workerProcessor(track)) ?? videoProcessor(track));
}

// Cached so repeated support checks don't each spawn a worker.
let probe: Promise<boolean> | undefined;

/** Whether this engine can capture via a native MediaStreamTrackProcessor in a worker. */
export async function workerSupported(): Promise<boolean> {
	probe ??= spawn().then((worker) => {
		worker?.close();
		return worker !== undefined;
	});

	return probe;
}

// Maps capture timestamps onto our wall clock so audio and video share one epoch. The first frame
// pins the two clocks together; everything after it keeps its spacing.
class Epoch {
	// The source timestamp we anchored on, and our wall clock at that moment in microseconds.
	#base?: number;
	#zero = 0;

	// Whether the source clock is stuck, decided on the second frame.
	#stuck?: boolean;

	// `at` is when the frame arrived, in milliseconds on our performance.now() timebase. For the
	// first frame it has to be measured as close to capture as possible: whatever delay sits between
	// capture and the anchor becomes a permanent audio/video offset.
	stamp(frame: VideoFrame, at: number): VideoFrame {
		const arrived = at * 1000;

		if (this.#base === undefined) {
			this.#base = frame.timestamp;
			this.#zero = arrived;
		} else if (this.#stuck === undefined) {
			// WebKit stamps every frame off a canvas capture track with 0, which would collapse the
			// whole stream onto one instant. One frame is enough to tell: a real capture clock always
			// advances, so anything that hasn't moved by now never will.
			this.#stuck = frame.timestamp === this.#base;
		}

		// A stuck source leaves arrival time as the only clock we have, which is what the <video>
		// fallback uses anyway.
		const timestamp = this.#stuck ? arrived : frame.timestamp - this.#base + this.#zero;

		const stamped = new VideoFrame(frame, { timestamp });
		frame.close();
		return stamped;
	}
}

// Stamps frames as they arrive, for pipelines that deliver straight to us.
function rewrite(): TransformStream<VideoFrame, VideoFrame> {
	const epoch = new Epoch();

	return new TransformStream<VideoFrame>({
		transform(frame, controller) {
			controller.enqueue(epoch.stamp(frame, performance.now()));
		},
	});
}

// Runs the native MediaStreamTrackProcessor in a worker, returning undefined if the engine doesn't
// have one (or won't transfer the track), in which case the caller falls back.
async function workerProcessor(source: StreamTrack): Promise<ReadableStream<VideoFrame> | undefined> {
	const worker = await spawn();
	if (!worker) return undefined;

	// Transfer a clone: the caller keeps its own handle for the preview, settings, and stop().
	const track = source.clone();

	try {
		// MediaStreamTrack is transferable wherever MediaStreamTrackProcessor is, but the DOM lib
		// doesn't say so yet.
		worker.post({ type: "start", track }, [track as unknown as Transferable]);
	} catch (err) {
		// Nothing was detached, so the clone is still ours to release.
		console.warn("moq-publish: MediaStreamTrack is not transferable", err);
		track.stop();
		worker.close();
		return undefined;
	}

	const epoch = new Epoch();

	return new ReadableStream<VideoFrame>({
		async pull(controller) {
			worker.post({ type: "pull" });

			const msg = await worker.next();
			switch (msg.type) {
				case "frame":
					// Anchor on when the worker read the frame, not when we got around to handling
					// the message, so a busy main thread can't skew video against audio.
					controller.enqueue(epoch.stamp(msg.frame, msg.at - performance.timeOrigin));
					return;
				case "done":
					worker.close();
					controller.close();
					return;
				default:
					worker.close();
					throw new Error(msg.type === "error" ? msg.message : `unexpected message: ${msg.type}`);
			}
		},
		cancel() {
			worker.close();
		},
	});
}

// A live capture worker, one request in flight at a time.
type Handle = {
	post(msg: ToWorker, transfer?: Transferable[]): void;
	next(): Promise<FromWorker>;
	close(): void;
};

// Starts a capture worker and waits for its support report, returning undefined when the engine has
// no MediaStreamTrackProcessor in a worker either.
async function spawn(): Promise<Handle | undefined> {
	let worker: Handle;

	try {
		worker = handle(new CaptureWorker());
	} catch (err) {
		// A strict CSP can refuse blob: workers, so treat it like an engine without the API.
		console.warn("moq-publish: failed to start the capture worker", err);
		return undefined;
	}

	const ready = await worker.next();
	if (ready.type !== "ready" || !ready.supported) {
		worker.close();
		return undefined;
	}

	return worker;
}

function handle(worker: Worker): Handle {
	// Messages can arrive before anybody asks for them (the initial "ready", or an error), so queue
	// them rather than dropping them on the floor.
	const queue: FromWorker[] = [];
	let waiting: ((msg: FromWorker) => void) | undefined;

	const push = (msg: FromWorker) => {
		const resolve = waiting;
		waiting = undefined;

		if (resolve) resolve(msg);
		else queue.push(msg);
	};

	worker.onmessage = (event: MessageEvent<FromWorker>) => push(event.data);
	worker.onerror = (event: ErrorEvent) => push({ type: "error", message: event.message });
	worker.onmessageerror = () => push({ type: "error", message: "failed to deserialize message" });

	return {
		post: (msg, transfer) => {
			worker.postMessage(msg, transfer ?? []);
		},
		// One request is in flight at a time (the stream serializes pulls), so a single slot is enough.
		next: () => {
			const msg = queue.shift();
			if (msg) return Promise.resolve(msg);
			return new Promise<FromWorker>((resolve) => {
				waiting = resolve;
			});
		},
		close: () => {
			// Cancelling mid-pull leaves a next() waiting on a worker that will never answer, so
			// settle it rather than stranding the promise and its closure.
			push({ type: "error", message: "worker terminated" });
			worker.terminate();
		},
	};
}

// The last resort: draw a <video> element into frames. It's gross and it stops producing (or worse,
// repeats the same picture) when the window isn't composited, so it's only for engines with no
// native MediaStreamTrackProcessor at all.
// Based on: https://jan-ivar.github.io/polyfills/mediastreamtrackprocessor.js
// Thanks Jan-Ivar
function videoProcessor(track: StreamTrack): ReadableStream<VideoFrame> {
	console.warn("Using MediaStreamTrackProcessor polyfill; performance might suffer.");

	let video: HTMLVideoElement;
	let handle: number | undefined;

	return new ReadableStream<VideoFrame>({
		async start() {
			video = document.createElement("video") as HTMLVideoElement;
			video.srcObject = new MediaStream([track]);
			await Promise.all([
				video.play(),
				new Promise((r) => {
					video.onloadedmetadata = r;
				}),
			]);
		},
		async pull(controller) {
			// requestVideoFrameCallback fires once per frame the camera actually delivers, so we
			// sample its true cadence rather than racing a wall clock: Safari and Firefox clamp
			// performance.now() to whole milliseconds, which can't express a 33.333ms period.
			await new Promise<void>((resolve) => {
				handle = video.requestVideoFrameCallback((now, metadata) => {
					// captureTime is the frame's capture instant; both it and now are on the
					// performance.now() timebase, so audio and video stay on one epoch.
					const timestamp = (metadata.captureTime ?? now) as Time.Milli;
					controller.enqueue(new VideoFrame(video, { timestamp: Time.Micro.fromMilli(timestamp) }));
					resolve();
				});
			});
		},
		cancel() {
			if (handle !== undefined) video.cancelVideoFrameCallback(handle);
			if (video) video.srcObject = null;
		},
	});
}

// Wraps a stream that can only be built asynchronously, without making the caller await it.
function deferred(open: () => Promise<ReadableStream<VideoFrame>>): ReadableStream<VideoFrame> {
	let reader: ReadableStreamDefaultReader<VideoFrame> | undefined;
	let cancelled = false;

	return new ReadableStream<VideoFrame>({
		// Nothing is pulled until this settles, so the source is always ready by the first pull.
		async start() {
			const stream = await open();

			// Cancelled while we were opening, so throw it away instead of reading from it.
			if (cancelled) {
				await stream.cancel();
				return;
			}

			reader = stream.getReader();
		},
		async pull(controller) {
			if (!reader) {
				controller.close();
				return;
			}

			const { value } = await reader.read();
			if (!value) controller.close();
			else controller.enqueue(value);
		},
		async cancel(reason) {
			cancelled = true;
			await reader?.cancel(reason);
		},
	});
}

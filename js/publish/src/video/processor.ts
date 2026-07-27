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
		// @ts-expect-error No typescript types yet.
		const input: ReadableStream<VideoFrame> = new self.MediaStreamTrackProcessor({ track }).readable;
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

// Rewrites capture timestamps onto our wall clock, anchoring the first frame to performance.now().
function rewrite(): TransformStream<VideoFrame, VideoFrame> {
	let base: number | undefined;
	let zero = 0;

	return new TransformStream<VideoFrame>({
		transform(frame, controller) {
			if (base === undefined) {
				base = frame.timestamp;
				zero = performance.now() * 1000;
			}
			const rewrite = new VideoFrame(frame, { timestamp: frame.timestamp - base + zero });
			frame.close();
			controller.enqueue(rewrite);
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

	const frames = new ReadableStream<VideoFrame>({
		async pull(controller) {
			worker.post({ type: "pull" });

			const msg = await worker.next();
			switch (msg.type) {
				case "frame":
					controller.enqueue(msg.frame);
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

	return frames.pipeThrough(rewrite());
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
		next: () => {
			const msg = queue.shift();
			if (msg) return Promise.resolve(msg);
			return new Promise<FromWorker>((resolve) => {
				waiting = resolve;
			});
		},
		close: () => {
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
			// sample its true cadence instead of racing a wall clock. The old timer settled at
			// 20fps for a 30fps camera because Safari/Firefox clamp performance.now() to whole
			// milliseconds, so a 33ms tick always read as "too early" for a 33.333ms period.
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

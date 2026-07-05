import * as Util from "@moq/hang/util";
import { Time } from "@moq/net";
import type { CaptureToHost } from "./capture-worker.ts";
import type { StreamTrack } from "./types";

/**
 * A ReadableStream of camera VideoFrames, with timestamps rewritten onto our wall clock.
 *
 * Prefers MediaStreamTrackProcessor: on the main thread in Chrome, and in a Worker on known Safari
 * 18+ (which only exposes it there). The Worker path keeps capturing while the publish window is
 * occluded. Every other engine (Firefox, older or version-less WebKit) falls back to a
 * requestAnimationFrame loop, which freezes when the page is hidden because the browser suspends rAF.
 */
export function TrackProcessor(track: StreamTrack): ReadableStream<VideoFrame> {
	// Chrome exposes MediaStreamTrackProcessor on the main thread.
	// @ts-expect-error MediaStreamTrackProcessor has no TypeScript types yet.
	if (self.MediaStreamTrackProcessor) {
		// @ts-expect-error MediaStreamTrackProcessor has no TypeScript types yet.
		const input: ReadableStream<VideoFrame> = new self.MediaStreamTrackProcessor({ track }).readable;
		return input.pipeThrough(rewriteTimestamps());
	}

	// Safari only exposes MediaStreamTrackProcessor inside a Worker, whose capture loop is not gated on
	// the main-thread render loop, so it survives window occlusion (unlike the rAF fallback below). Take
	// this path on WebKit 18+ (safariWorkerCapture), which has both worker-scope MediaStreamTrackProcessor
	// and transferable MediaStreamTrack. That includes iOS Chrome/Firefox on iOS 18+ (all WebKit, detected
	// via the iOS OS version). Anything we can't confirm is 18+ uses rAF rather than risk a doomed worker.
	if (Util.Hacks.safariWorkerCapture) {
		return workerTrackProcessor(track).pipeThrough(rewriteTimestamps());
	}

	return rafTrackProcessor(track);
}

/** Rewrite frame timestamps onto our wall clock so audio and video share one epoch. */
function rewriteTimestamps(): TransformStream<VideoFrame, VideoFrame> {
	let base: number | undefined;
	let zero = 0;

	return new TransformStream<VideoFrame, VideoFrame>({
		transform(frame, controller) {
			if (base === undefined) {
				base = frame.timestamp;
				zero = performance.now() * 1000;
			}
			const rewritten = new VideoFrame(frame, { timestamp: frame.timestamp - base + zero });
			frame.close();
			controller.enqueue(rewritten);
		},
	});
}

/**
 * Capture VideoFrames via MediaStreamTrackProcessor running in a Worker. Only reached on known Safari
 * 18+ (see {@link TrackProcessor}); the worker keeps producing frames while the publish window is
 * occluded, which the main-thread rAF path cannot.
 */
function workerTrackProcessor(track: StreamTrack): ReadableStream<VideoFrame> {
	let worker: Worker | undefined;
	// settled: the stream has closed, errored, or been cancelled, so no further chunks may be enqueued
	// and the worker should be gone.
	let settled = false;

	const teardown = () => {
		worker?.terminate();
		worker = undefined;
	};

	return new ReadableStream<VideoFrame>({
		async start(controller) {
			// Load the worker lazily. A static import would pull the ?worklet module into the eager
			// capture graph, which breaks non-Vite loaders like `bun test` that lack the plugin.
			const { default: workerUrl } = await import("./capture-worker.ts?worklet");

			// cancel() can run while the import above is still pending (the Streams spec does not gate
			// cancel on start settling); it sets `settled`. No worker exists yet, so bail before spawning
			// one nothing would stop. There is no further await below, so this is the only window cancel
			// can sneak through.
			if (settled) return;

			worker = new Worker(workerUrl, { type: "module" });

			worker.onmessage = (event: MessageEvent<CaptureToHost>) => {
				const message = event.data;

				if (settled) {
					// A frame already in flight when we closed/errored: close it so it doesn't leak.
					if (message.type === "frame") message.frame.close();
					return;
				}

				switch (message.type) {
					case "frame":
						// The worker captures an independent clone, so mirror the caller's track state here;
						// the Chrome main-thread path reads the original track and gets this for free. A
						// stopped source ends the stream (matching Chrome). A muted one (enabled === false)
						// drops frames: this diverges from Chrome, which delivers black frames for a disabled
						// track, so remote watchers see the last frame freeze rather than go black. Privacy
						// holds either way; we drop rather than synthesize a black frame to stay simple.
						if (track.readyState === "ended") {
							message.frame.close();
							settled = true;
							controller.close();
							teardown();
							return;
						}
						// Drop rather than buffer when muted or when the consumer is behind. This only bounds
						// frames the host has already received: the worker reads at full rate with no
						// backpressure, so a stalled main thread can still queue transferred frames in the
						// message channel ahead of this check.
						if (!track.enabled || (controller.desiredSize ?? 1) <= 0) {
							message.frame.close();
							return;
						}
						controller.enqueue(message.frame);
						break;
					case "done":
						settled = true;
						controller.close();
						teardown();
						break;
					case "error":
						settled = true;
						controller.error(new Error(`capture worker: ${message.message}`));
						teardown();
						break;
				}
			};
			worker.onerror = (event) => {
				if (settled) return;
				settled = true;
				// ErrorEvent.message is "" (not null) for opaque/load failures, so use || not ??.
				controller.error(new Error(`capture worker crashed: ${event.message || "unknown"}`));
				teardown();
			};

			// Clone so transferring into the Worker never neuters the caller's track; the clone shares the
			// same camera source, so there is no second capture. On teardown the host terminates the
			// worker, and the browser ends the transferred clone when the worker global is destroyed (so
			// the camera indicator turns off).
			const clone = track.clone();
			try {
				worker.postMessage({ track: clone }, [clone as unknown as Transferable]);
			} catch (err) {
				// Transferable MediaStreamTrack ships in every Safari that reaches this path, so a throw
				// here is unexpected. Stop the un-moved clone and surface the failure.
				clone.stop();
				settled = true;
				controller.error(err instanceof Error ? err : new Error(String(err)));
				teardown();
			}
		},
		cancel() {
			settled = true;
			teardown();
		},
	});
}

/**
 * Last-resort capture for engines with no usable MediaStreamTrackProcessor: an HTMLVideoElement paced
 * by requestAnimationFrame. The browser suspends rAF when the page is hidden or occluded, so this
 * freezes in the background; prefer the Worker path above wherever possible.
 */
function rafTrackProcessor(track: StreamTrack): ReadableStream<VideoFrame> {
	console.warn("Using MediaStreamTrackProcessor polyfill; performance might suffer.");

	const settings = track.getSettings();
	if (!settings) {
		throw new Error("track has no settings");
	}

	let video: HTMLVideoElement | undefined;
	let last: Time.Milli;
	let cancelled = false;

	const frameRate = settings.frameRate ?? 30;

	// Stop the pull loop and release the hidden <video> so it stops decoding the camera. Idempotent,
	// and called from cancel() and every error path: a ReadableStream does NOT invoke cancel() when
	// start()/pull() reject, so without this the element would keep decoding until GC.
	const release = () => {
		cancelled = true;
		if (video) {
			video.pause();
			video.srcObject = null;
			video = undefined;
		}
	};

	return new ReadableStream<VideoFrame>({
		async start() {
			const el = document.createElement("video") as HTMLVideoElement;
			video = el;
			el.srcObject = new MediaStream([track]);
			try {
				await Promise.all([
					el.play(),
					new Promise((r) => {
						el.onloadedmetadata = r;
					}),
				]);
			} catch (err) {
				release();
				throw err;
			}

			last = Time.Milli.now();
		},
		async pull(controller) {
			try {
				while (!cancelled) {
					// The source track can end underneath us (camera stopped/unplugged). The hidden <video>
					// would otherwise keep handing back its last frame forever, so close the stream cleanly.
					if (!video || track.readyState === "ended") {
						controller.close();
						release();
						return;
					}

					const now = Time.Milli.now();
					if (Time.Milli.sub(now, last) < ((1000 / frameRate) as Time.Milli)) {
						await new Promise((r) => requestAnimationFrame(r));
						continue;
					}

					last = now;
					controller.enqueue(new VideoFrame(video, { timestamp: Time.Micro.fromMilli(last) }));
				}
			} catch (err) {
				// new VideoFrame() throws once the <video> loses its current frame (e.g. the track ended
				// mid-pull). Release and surface the error instead of leaking the element.
				release();
				throw err;
			}
		},
		cancel() {
			release();
		},
	});
}

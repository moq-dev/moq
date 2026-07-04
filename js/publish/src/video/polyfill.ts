import * as Util from "@moq/hang/util";
import { Time } from "@moq/net";
import type { StreamTrack } from "./types";

/**
 * A ReadableStream of camera VideoFrames, with timestamps rewritten onto our wall clock.
 *
 * Prefers MediaStreamTrackProcessor: on the main thread in Chrome, and in a Worker on Safari (which
 * only exposes it there). The Worker path keeps capturing while the publish window is occluded; the
 * requestAnimationFrame fallback (last resort) freezes in that case because the browser suspends rAF.
 */
export function TrackProcessor(track: StreamTrack): ReadableStream<VideoFrame> {
	// Chrome exposes MediaStreamTrackProcessor on the main thread.
	// @ts-expect-error MediaStreamTrackProcessor has no TypeScript types yet.
	if (self.MediaStreamTrackProcessor) {
		// @ts-expect-error MediaStreamTrackProcessor has no TypeScript types yet.
		const input: ReadableStream<VideoFrame> = new self.MediaStreamTrackProcessor({ track }).readable;
		return input.pipeThrough(rewriteTimestamps());
	}

	// Safari only exposes MediaStreamTrackProcessor inside a Worker, whose capture loop is not gated
	// on the main-thread render loop, so it survives window occlusion (unlike the rAF fallback below).
	// Firefox also supports Worker MediaStreamTrackProcessor and could move here later.
	if (Util.Hacks.isSafari) {
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

/** Capture VideoFrames via MediaStreamTrackProcessor running in a Worker (Safari). */
function workerTrackProcessor(track: StreamTrack): ReadableStream<VideoFrame> {
	let worker: Worker | undefined;

	return new ReadableStream<VideoFrame>({
		async start(controller) {
			// Load the worker lazily. A static import would pull the ?worklet module into the eager
			// capture graph, which breaks non-Vite loaders like `bun test` that lack the plugin.
			const { default: workerUrl } = await import("./capture-worker.ts?worklet");
			worker = new Worker(workerUrl, { type: "module" });

			worker.onmessage = (event: MessageEvent<{ frame?: VideoFrame; error?: string }>) => {
				if (event.data.frame) {
					controller.enqueue(event.data.frame);
				} else if (event.data.error) {
					controller.error(new Error(`capture worker: ${event.data.error}`));
				}
			};
			worker.onerror = () => controller.error(new Error("capture worker crashed"));

			// Clone so transferring into the Worker never neuters the caller's track; the clone shares
			// the same camera source, so there is no second capture.
			const clone = track.clone();
			worker.postMessage({ track: clone }, [clone as unknown as Transferable]);
		},
		cancel() {
			worker?.terminate();
		},
	});
}

/**
 * Last-resort capture for engines with no MediaStreamTrackProcessor: an HTMLVideoElement paced by
 * requestAnimationFrame. The browser suspends rAF when the page is hidden or occluded, so this
 * freezes in the background; prefer the Worker path above wherever possible.
 */
function rafTrackProcessor(track: StreamTrack): ReadableStream<VideoFrame> {
	console.warn("Using MediaStreamTrackProcessor polyfill; performance might suffer.");

	const settings = track.getSettings();
	if (!settings) {
		throw new Error("track has no settings");
	}

	let video: HTMLVideoElement;
	let last: Time.Milli;

	const frameRate = settings.frameRate ?? 30;

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

			last = Time.Milli.now();
		},
		async pull(controller) {
			while (true) {
				const now = Time.Milli.now();
				if (Time.Milli.sub(now, last) < ((1000 / frameRate) as Time.Milli)) {
					await new Promise((r) => requestAnimationFrame(r));
					continue;
				}

				last = now;
				controller.enqueue(new VideoFrame(video, { timestamp: Time.Micro.fromMilli(last) }));
			}
		},
	});
}

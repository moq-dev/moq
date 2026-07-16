import { Time } from "@moq/net";
import type { StreamTrack } from "./types";

// Firefox doesn't support MediaStreamTrackProcessor so we need to use a polyfill.
// Based on: https://jan-ivar.github.io/polyfills/mediastreamtrackprocessor.js
// Thanks Jan-Ivar
export function TrackProcessor(track: StreamTrack): ReadableStream<VideoFrame> {
	// @ts-expect-error No typescript types yet.
	if (self.MediaStreamTrackProcessor) {
		// Rewrite timestamps onto our wall clock so audio and video share one epoch.
		let base: number | undefined;
		let zero = 0;

		const rewrite = new TransformStream<VideoFrame>({
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

		// @ts-expect-error No typescript types yet.
		const input: ReadableStream<VideoFrame> = new self.MediaStreamTrackProcessor({ track }).readable;
		return input.pipeThrough(rewrite);
	}

	// TODO Firefox supports this in a background worker.
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

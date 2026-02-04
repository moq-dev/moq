import { Time } from "@moq/lite";
import type { StreamTrack } from "./types";

// Firefox doesn't support MediaStreamTrackProcessor so we need to use a polyfill.
// Based on: https://jan-ivar.github.io/polyfills/mediastreamtrackprocessor.js
// Thanks Jan-Ivar
export function TrackProcessor(track: StreamTrack): ReadableStream<VideoFrame> {
	// @ts-expect-error No typescript types yet.
	if (self.MediaStreamTrackProcessor) {
		// Rewrite timestamps so they use our wall clock time instead of starting at 0.
		// TODO verify all browsers actually start at 0.
		const zero = performance.now() * 1000;

		const rewrite = new TransformStream<VideoFrame>({
			transform(frame, controller) {
				const rewrite = new VideoFrame(frame, { timestamp: frame.timestamp + zero });
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

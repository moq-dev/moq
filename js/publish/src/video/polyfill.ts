import { Time } from "@moq/net";
import type { StreamTrack } from "./types";

/**
 * Paces the capture loop below against a frame deadline.
 *
 * Exported for the unit test; not part of the package's public API.
 */
export class Pacer {
	// Safari and Firefox clamp performance.now() to 1ms, so a 33.333ms period never looks elapsed while
	// the clock still reports 33. Allow the clock's own resolution as slack, otherwise we wait for one
	// more animation frame and capture 20fps out of a 30fps camera.
	static readonly SLACK = Time.Milli(1);

	readonly #period: Time.Milli;
	#next: Time.Milli;

	constructor(frameRate: number, now: Time.Milli = Time.Milli.now()) {
		this.#period = Time.Milli(1000 / frameRate);
		this.#next = now;
	}

	/** True when the next frame is due, advancing the deadline past it. */
	due(now: Time.Milli): boolean {
		if (Time.Milli.sub(this.#next, now) > Pacer.SLACK) return false;

		// Advance by exactly one period so the rounding error can't accumulate. Resync rather than build
		// up a backlog when we've been stalled, since a hidden tab suspends requestAnimationFrame.
		this.#next = Time.Milli.add(this.#next, this.#period);
		if (this.#next < now) this.#next = Time.Milli.add(now, this.#period);

		return true;
	}
}

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

	const settings = track.getSettings();
	if (!settings) {
		throw new Error("track has no settings");
	}

	let video: HTMLVideoElement | undefined;
	const pacer = new Pacer(settings.frameRate ?? 30);

	const release = () => {
		if (!video) return;
		video.pause();
		video.srcObject = null;
		video = undefined;
	};

	return new ReadableStream<VideoFrame>({
		async start() {
			const el = document.createElement("video") as HTMLVideoElement;
			video = el;

			el.srcObject = new MediaStream([track]);
			await Promise.all([
				el.play(),
				new Promise((r) => {
					el.onloadedmetadata = r;
				}),
			]);
		},
		async pull(controller) {
			for (;;) {
				// The track can end underneath us (ex. the camera is unplugged), and the <video> would
				// otherwise keep handing back its final frame forever.
				if (!video || track.readyState === "ended") {
					controller.close();
					release();
					return;
				}

				const now = Time.Milli.now();
				if (!pacer.due(now)) {
					await new Promise((r) => requestAnimationFrame(r));
					continue;
				}

				controller.enqueue(new VideoFrame(video, { timestamp: Time.Micro.fromMilli(now) }));
				return;
			}
		},
		cancel() {
			release();
		},
	});
}

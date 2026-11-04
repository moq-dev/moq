import { Micro, Second } from "@moq/clock";
import * as Util from "@moq/hang/util";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type { InputAudioTrack, InputVideoTrack, VideoSample } from "mediabunny";
import { now, pace, sleepUntil } from "./pace";
import { Timeline } from "./timeline";
import type { Media } from "./types";

/** Signals the file source reads. */
export type FileInput = {
	// Whether to decode the file. Defaults to true. When false the decode stops and `out.source` clears.
	enabled: Getter<boolean>;
};

/** Constructor options: the wired inputs plus the file to decode. */
export interface FileProps extends Inputs<FileInput> {
	/** Seed the file; also live-editable via `file.file` and {@link File.prompt}. */
	file?: globalThis.File | Signal<globalThis.File | undefined>;
}

/** Signals the file source derives. */
export type FileOutput = {
	// The sources decoded from the file, undefined while disabled or undecodable.
	source: Signal<Media | undefined>;
};

// Image, video, and audio files we know how to decode (see #decode).
const ACCEPT = "image/*,video/*,audio/*";

// A still image has no cadence of its own, so republish it at this rate. Delta coding makes the
// repeats nearly free, and a steady stream keeps the encoder's keyframe interval honest.
const IMAGE_FRAME_RATE = 30;

// How many packets to measure when estimating the file's frame rate.
const FRAME_RATE_SAMPLES = 100;

/**
 * Decodes a local file into looping publish sources.
 *
 * The file is demuxed and decoded directly rather than played through a `<video>` element, so the
 * frames carry the container's own timestamps, audio works on every browser, and nothing stops when
 * the window isn't composited.
 */
export class File {
	/** The wired inputs, read-only. */
	readonly in: Readonlys<FileInput>;

	/** The file to decode. Written by {@link prompt}, or set it directly. */
	file: Signal<globalThis.File | undefined>;

	readonly #out: FileOutput = {
		source: new Signal<Media | undefined>(undefined),
	};
	/** The derived outputs, read-only. */
	readonly out = readonlys(this.#out);

	#signals = new Effect();

	constructor(props?: FileProps) {
		this.in = {
			enabled: getter(props?.enabled ?? true),
		};
		this.file = Signal.from(props?.file);

		this.#signals.run((effect) => {
			const values = effect.getAll([this.file, this.in.enabled]);
			if (!values) return;
			const [file] = values;

			this.#decode(file, effect).catch((err) => {
				console.error("Failed to decode file:", err);
			});
		});
	}

	/**
	 * Open a native file picker and use the chosen file as the source.
	 * Must be called from within a user gesture (e.g. a click handler), otherwise the browser blocks the dialog.
	 */
	prompt() {
		const input = document.createElement("input");
		input.type = "file";
		input.accept = ACCEPT;
		input.addEventListener("change", () => {
			const file = input.files?.[0];
			if (file) this.file.set(file);
		});
		input.click();
	}

	async #decode(file: globalThis.File, effect: Effect) {
		// Only images are routed by MIME type. Everything else goes to the demuxer, which sniffs the
		// container itself and so still works for the files browsers report with an empty type.
		if (file.type.startsWith("image/")) {
			await this.#decodeImage(file, effect);
		} else {
			await this.#decodeMedia(file, effect);
		}
	}

	async #decodeImage(file: globalThis.File, effect: Effect) {
		const signal = effect.abort;

		const bitmap = await createImageBitmap(file);
		effect.cleanup(() => bitmap.close());
		if (signal.aborted) return;

		const zero = now();
		let index = 0;

		const frames = new ReadableStream<VideoFrame>(
			{
				async pull(controller) {
					// Every repeat is the same picture, so a reader returning after a gap skips to the
					// slot due now instead of replaying each one it missed.
					const due = Math.floor(Second.fromMicro((now() - zero) as Micro) * IMAGE_FRAME_RATE);
					index = Math.max(index, due);

					const offset = Micro.fromSecond((index / IMAGE_FRAME_RATE) as Second);
					const timestamp = (zero + offset) as Micro;
					index++;

					await sleepUntil(timestamp, signal);

					// The bitmap is closed on teardown, so stop before building a frame from a dead one.
					if (signal.aborted) {
						controller.close();
						return;
					}

					controller.enqueue(new VideoFrame(bitmap, { timestamp }));
				},
			},
			// Prefetching would park a stale frame in the queue while nothing reads, and hand it out on resume.
			{ highWaterMark: 0 },
		);

		effect.set(this.#out.source, { video: { frames, frameRate: IMAGE_FRAME_RATE } }, undefined);
	}

	async #decodeMedia(file: globalThis.File, effect: Effect) {
		const signal = effect.abort;
		// Keep the picker and image path available without downloading the media demuxers.
		const { ALL_FORMATS, AudioSampleSink, BlobSource, Input, VideoSampleSink } = await import("mediabunny");
		if (signal.aborted) return;

		const input = new Input({ source: new BlobSource(file), formats: ALL_FORMATS });
		effect.cleanup(() => input.dispose());

		if (!(await input.canRead())) {
			// Disposing the input mid-read is a teardown, not a broken file.
			if (signal.aborted) return;
			throw new Error(`Cannot publish "${file.name}": unrecognized container format.`);
		}

		const [video, audio] = await Promise.all([input.getPrimaryVideoTrack(), input.getPrimaryAudioTrack()]);

		// Install the WebCodecs polyfill before probing: canDecode() reports false when AudioDecoder
		// is missing rather than throwing, so probing first would drop the audio track on an engine
		// the polyfill could have covered. A no-op wherever WebCodecs is native.
		if (audio) await Util.Libav.polyfill();

		// A codec this browser can't decode drops just its own track: the other one is usually still
		// publishable, and a video-only publish beats failing outright.
		const [videoOk, audioOk] = await Promise.all([decodable(video, file), decodable(audio, file)]);
		if (!videoOk && !audioOk) {
			if (signal.aborted) return;
			throw new Error(`Cannot publish "${file.name}": no decodable audio or video track.`);
		}

		const duration = await input.computeDuration();

		// The encoder sizes its bitrate off this, and a file's real cadence beats assuming 30fps.
		const frameRate =
			video && videoOk ? (await video.computePacketStats(FRAME_RATE_SAMPLES)).averagePacketRate : undefined;
		const toFrame = video && videoOk ? await converter(video) : undefined;
		const format =
			audio && audioOk
				? { sampleRate: await audio.getSampleRate(), channelCount: await audio.getNumberOfChannels() }
				: undefined;

		// Pin the clock last. Probing the file takes a moment, and anchoring before that would leave
		// the first samples already overdue, so playback would open with a catch-up burst.
		//
		// One timeline for both tracks, so they restart together and stay in sync across loops.
		const timeline = new Timeline(now(), duration);

		const source: Media = {
			video:
				video && toFrame
					? {
							frames: pace(new VideoSampleSink(video), timeline, toFrame, signal),
							frameRate,
						}
					: undefined,
			audio:
				audio && format
					? {
							samples: pace(
								new AudioSampleSink(audio),
								timeline,
								(sample) => sample.toAudioData(),
								signal,
							),
							...format,
							// A file is whatever the user picked, so leave the Opus tuning to the encoder.
							kind: "auto",
						}
					: undefined,
		};

		if (signal.aborted) return;
		effect.set(this.#out.source, source, undefined);
	}

	/** Stop decoding and release the file. */
	close() {
		this.#signals.close();
	}
}

// Phone footage is usually stored sideways with rotation metadata, which toVideoFrame() ignores
// because a VideoFrame can't carry it. Drawing through a canvas is the only way to bake it in.
async function converter(track: InputVideoTrack): Promise<(sample: VideoSample) => VideoFrame> {
	if ((await track.getRotation()) === 0) return (sample) => sample.toVideoFrame();

	const canvas = new OffscreenCanvas(await track.getDisplayWidth(), await track.getDisplayHeight());

	const ctx = canvas.getContext("2d");
	if (!ctx) throw new Error("Failed to create 2D canvas context");

	return (sample) => {
		sample.draw(ctx, 0, 0, canvas.width, canvas.height);
		return new VideoFrame(canvas, { timestamp: sample.microsecondTimestamp });
	};
}

// Whether we can decode this track, warning (rather than throwing) when we can't so the file's other
// track still publishes.
async function decodable(track: InputVideoTrack | InputAudioTrack | null, file: globalThis.File): Promise<boolean> {
	if (!track) return false;
	if (await track.canDecode()) return true;

	console.warn(
		`Publishing "${file.name}" without its ${track.type} track: this browser can't decode ${await track.getCodec()}.`,
	);
	return false;
}

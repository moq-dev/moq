import type * as Moq from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import * as Audio from "./audio";
import type { Broadcast } from "./broadcast";
import { Muxer } from "./mse";
import { Sync } from "./sync";
import * as Video from "./video";

// Serializable representation of TimeRanges
export interface BufferedRange {
	start: number; // seconds
	end: number; // seconds
}
export type BufferedRanges = BufferedRange[];

// Helper to convert DOM TimeRanges
export function timeRangesToArray(ranges: TimeRanges): BufferedRanges {
	const result: BufferedRange[] = [];
	for (let i = 0; i < ranges.length; i++) {
		result.push({ start: ranges.start(i), end: ranges.end(i) });
	}
	return result;
}

export interface Backend {
	// Whether audio/video playback is paused.
	paused: Signal<boolean>;

	// Whether the video is currently buffering, false when paused.
	buffering: Getter<boolean>;

	// Current playback position in seconds.
	timestamp: Getter<number>;

	// Video specific signals.
	video?: Video.Backend;

	// Audio specific signals.
	audio?: Audio.Backend;

	// The jitter in milliseconds required for smooth playback.
	jitter: Signal<Moq.Time.Milli>;
}

export interface MultiBackendProps {
	element?: HTMLCanvasElement | HTMLVideoElement | Signal<HTMLCanvasElement | HTMLVideoElement | undefined>;
	broadcast?: Broadcast | Signal<Broadcast | undefined>;

	// Additional jitter in milliseconds on top of catalog jitter.
	jitter?: Moq.Time.Milli | Signal<Moq.Time.Milli>;

	paused?: boolean | Signal<boolean>;
}

// We have to proxy some of these signals because we support both the MSE and WebCodecs.
class VideoBackend implements Video.Backend {
	// The source of the video.
	source: Video.Source;

	// The stats of the video.
	stats = new Signal<Video.Stats | undefined>(undefined);

	// Buffered time ranges (for MSE backend).
	buffered = new Signal<BufferedRanges>([]);

	constructor(source: Video.Source) {
		this.source = source;
	}
}

// Audio specific signals that work regardless of the backend source (mse vs webcodecs).
class AudioBackend implements Audio.Backend {
	source: Audio.Source;

	// The volume of the audio, between 0 and 1.
	volume = new Signal<number>(0.5);

	// Whether the audio is muted.
	muted = new Signal<boolean>(false);

	// The stats of the audio.
	stats = new Signal<Audio.Stats | undefined>(undefined);

	// Buffered time ranges (for MSE backend).
	buffered = new Signal<BufferedRanges>([]);

	constructor(source: Audio.Source) {
		this.source = source;
	}
}

/// A generic backend that supports either MSE or WebCodecs based on the provided element.
///
/// This is primarily what backs the <hang-watch> web component, but it's useful as a standalone for other use cases.
export class MultiBackend implements Backend {
	element = new Signal<HTMLCanvasElement | HTMLVideoElement | undefined>(undefined);
	broadcast: Signal<Broadcast | undefined>;
	jitter: Signal<Moq.Time.Milli>;
	paused: Signal<boolean>;

	video: VideoBackend;
	#videoSource: Video.Source;

	audio: AudioBackend;
	#audioSource: Audio.Source;

	// Used to sync audio and video playback at a target delay.
	#sync: Sync;

	#buffering = new Signal<boolean>(false);
	readonly buffering: Getter<boolean> = this.#buffering;

	#timestamp = new Signal<number>(0);
	readonly timestamp: Getter<number> = this.#timestamp;

	signals = new Effect();

	constructor(props?: MultiBackendProps) {
		this.element = Signal.from(props?.element);
		this.broadcast = Signal.from(props?.broadcast);
		this.jitter = Signal.from(props?.jitter ?? (100 as Moq.Time.Milli));
		this.#sync = new Sync({ jitter: this.jitter });

		this.#videoSource = new Video.Source(this.#sync, {
			broadcast: this.broadcast,
		});
		this.#audioSource = new Audio.Source(this.#sync, {
			broadcast: this.broadcast,
		});

		this.video = new VideoBackend(this.#videoSource);
		this.audio = new AudioBackend(this.#audioSource);

		this.paused = Signal.from(props?.paused ?? false);

		this.signals.effect(this.#runElement.bind(this));
	}

	#runElement(effect: Effect): void {
		const element = effect.get(this.element);
		if (!element) return;

		if (element instanceof HTMLCanvasElement) {
			this.#runWebcodecs(effect, element);
		} else if (element instanceof HTMLVideoElement) {
			this.#runMse(effect, element);
		}
	}

	#runWebcodecs(effect: Effect, element: HTMLCanvasElement): void {
		const videoSource = new Video.Decoder(this.#videoSource);
		const audioSource = new Audio.Decoder(this.#audioSource);

		const audioEmitter = new Audio.Emitter(audioSource, {
			volume: this.audio.volume,
			muted: this.audio.muted,
			paused: this.paused,
		});

		const videoRenderer = new Video.Renderer(videoSource, { canvas: element, paused: this.paused });

		effect.cleanup(() => {
			videoSource.close();
			audioSource.close();
			audioEmitter.close();
			videoRenderer.close();
		});

		// Proxy the read only signals to the backend.
		effect.proxy(this.video.stats, videoSource.stats);
		effect.proxy(this.video.buffered, videoSource.buffered);

		effect.proxy(this.audio.stats, audioSource.stats);
		effect.proxy(this.audio.buffered, audioSource.buffered);

		// Derive timestamp from video stats (in lock-step with frame signal)
		effect.effect((e) => {
			const stats = e.get(videoSource.stats);
			if (stats) {
				this.#timestamp.set(stats.timestamp / 1_000_000); // microseconds to seconds
			}
		});
	}

	#runMse(effect: Effect, element: HTMLVideoElement): void {
		const mse = new Muxer(this.#sync, {
			jitter: this.jitter,
			paused: this.paused,
			element,
		});
		effect.cleanup(() => mse.close());

		const video = new Video.Mse(mse, this.#videoSource);
		const audio = new Audio.Mse(mse, this.#audioSource, {
			volume: this.audio.volume,
			muted: this.audio.muted,
		});

		// Proxy the read only signals to the backend.
		effect.proxy(this.video.stats, video.stats);
		effect.proxy(this.video.buffered, video.buffered);

		effect.proxy(this.audio.stats, audio.stats);
		effect.proxy(this.audio.buffered, audio.buffered);

		effect.proxy(this.#timestamp, mse.timestamp);
	}
}

import type * as Moq from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import type * as Catalog from "../catalog";
import * as Audio from "./audio";
import type { Broadcast } from "./broadcast";
import * as MSE from "./mse";
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
	video: Video.Backend;

	// Audio specific signals.
	audio: Audio.Backend;

	// The delay in milliseconds required for smooth playback.
	delay: Signal<Moq.Time.Milli>;
}

export interface MultiBackendProps {
	element?: HTMLCanvasElement | HTMLVideoElement | Signal<HTMLCanvasElement | HTMLVideoElement | undefined>;
	broadcast?: Broadcast | Signal<Broadcast | undefined>;

	// Additional delay in milliseconds on top of catalog delay.
	delay?: Moq.Time.Milli | Signal<Moq.Time.Milli>;

	paused?: boolean | Signal<boolean>;
}

// We have to proxy some of these signals because we support both the MSE and WebCodecs.
class VideoSignals implements Video.Backend {
	// The desired size/rendition/bitrate of the video.
	target = new Signal<Video.Target | undefined>(undefined);

	// The catalog of the video.
	catalog = new Signal<Catalog.Video | undefined>(undefined);

	// The name of the active rendition.
	rendition = new Signal<string | undefined>(undefined);

	// The stats of the video.
	stats = new Signal<Video.Stats | undefined>(undefined);

	// The config of the active rendition.
	config = new Signal<Catalog.VideoConfig | undefined>(undefined);

	// Buffered time ranges for MSE backend.
	buffered = new Signal<BufferedRanges>([]);
}

// Audio specific signals that work regardless of the backend source (mse vs webcodecs).
class AudioSignals implements Audio.Backend {
	// The volume of the audio, between 0 and 1.
	volume = new Signal(0.5);

	// Whether the audio is muted.
	muted = new Signal(false);

	// The desired rendition/bitrate of the audio.
	target = new Signal<Audio.Target | undefined>(undefined);

	// The catalog of the audio.
	catalog = new Signal<Catalog.Audio | undefined>(undefined);

	// The name of the active rendition.
	rendition = new Signal<string | undefined>(undefined);

	// The config of the active rendition.
	config = new Signal<Catalog.AudioConfig | undefined>(undefined);

	// The stats of the audio.
	stats = new Signal<Audio.Stats | undefined>(undefined);

	// Buffered time ranges for MSE backend.
	buffered = new Signal<BufferedRanges>([]);
}

/// A generic backend that supports either MSE or WebCodecs based on the provided element.
///
/// This is primarily what backs the <hang-watch> web component, but it's useful as a standalone for other use cases.
export class MultiBackend implements Backend {
	element = new Signal<HTMLCanvasElement | HTMLVideoElement | undefined>(undefined);
	broadcast: Signal<Broadcast | undefined>;
	delay: Signal<Moq.Time.Milli>;
	paused: Signal<boolean>;

	video = new VideoSignals();
	audio = new AudioSignals();

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
		this.delay = Signal.from(props?.delay ?? (100 as Moq.Time.Milli));

		this.#sync = new Sync({ delay: this.delay });

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
		const videoSource = new Video.Decoder({
			broadcast: this.broadcast,
			target: this.video.target,
			sync: this.#sync,
		});
		const audioSource = new Audio.Decoder({
			broadcast: this.broadcast,
			target: this.audio.target,
			sync: this.#sync,
		});

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
		effect.proxy(this.video.catalog, videoSource.catalog);
		effect.proxy(this.video.rendition, videoSource.rendition);
		effect.proxy(this.video.config, videoSource.config);
		effect.proxy(this.video.stats, videoSource.stats);
		effect.proxy(this.video.buffered, videoSource.buffered);

		effect.proxy(this.audio.catalog, audioSource.catalog);
		effect.proxy(this.audio.rendition, audioSource.rendition);
		effect.proxy(this.audio.config, audioSource.config);
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
		const source = new MSE.Source({
			broadcast: this.broadcast,
			delay: this.delay,
			element,
			paused: this.paused,
			video: { target: this.video.target },
			audio: { volume: this.audio.volume, muted: this.audio.muted },
		});
		effect.cleanup(() => source.close());

		// Proxy the read only signals to the backend.
		effect.proxy(this.video.catalog, source.video.catalog);
		effect.proxy(this.video.rendition, source.video.rendition);
		effect.proxy(this.video.config, source.video.config);
		effect.proxy(this.video.stats, source.video.stats);
		effect.proxy(this.video.buffered, source.video.buffered);

		effect.proxy(this.audio.catalog, source.audio.catalog);
		effect.proxy(this.audio.rendition, source.audio.rendition);
		effect.proxy(this.audio.config, source.audio.config);
		effect.proxy(this.audio.stats, source.audio.stats);
		effect.proxy(this.audio.buffered, source.audio.buffered);

		effect.proxy(this.#timestamp, source.timestamp);
	}
}

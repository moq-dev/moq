import type * as Moq from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import type * as Catalog from "../catalog";
import * as Audio from "./audio";
import type { Broadcast } from "./broadcast";
import * as MSE from "./mse";
import * as Video from "./video";

export interface Backend {
	// Delay playing audio and video by this amount, skipping frames if necessary.
	latency: Signal<Moq.Time.Milli>;

	// Whether audio/video playback is paused.
	paused: Signal<boolean>;

	// Whether the video is currently buffering, false when paused.
	buffering: Getter<boolean>;

	// Video specific signals.
	video: Video.Backend;

	// Audio specific signals.
	audio: Audio.Backend;
}

export interface CombinedProps {
	element?: HTMLCanvasElement | HTMLVideoElement | Signal<HTMLCanvasElement | HTMLVideoElement | undefined>;
	broadcast?: Broadcast | Signal<Broadcast | undefined>;

	latency?: Moq.Time.Milli | Signal<Moq.Time.Milli>;
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
}

/// A generic backend that supports either MSE or WebCodecs based on the provided element.
///
/// This is primarily what backs the <hang-watch> web component, but it's useful as a standalone for other use cases.
export class Combined implements Backend {
	element = new Signal<HTMLCanvasElement | HTMLVideoElement | undefined>(undefined);
	broadcast: Signal<Broadcast | undefined>;
	latency: Signal<Moq.Time.Milli>;
	paused: Signal<boolean>;

	video = new VideoSignals();
	audio = new AudioSignals();

	#buffering = new Signal<boolean>(false);
	readonly buffering: Getter<boolean> = this.#buffering;

	signals = new Effect();

	constructor(props?: CombinedProps) {
		this.element = Signal.from(props?.element);
		this.broadcast = Signal.from(props?.broadcast);

		this.latency = Signal.from(props?.latency ?? (100 as Moq.Time.Milli));
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
		const videoSource = new Video.Source({
			broadcast: this.broadcast,
			latency: this.latency,
			target: this.video.target,
		});
		const audioSource = new Audio.Source({
			broadcast: this.broadcast,
			latency: this.latency,
			target: this.audio.target,
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

		effect.proxy(this.audio.catalog, audioSource.catalog);
		effect.proxy(this.audio.rendition, audioSource.rendition);
		effect.proxy(this.audio.config, audioSource.config);
		effect.proxy(this.audio.stats, audioSource.stats);
	}

	#runMse(effect: Effect, element: HTMLVideoElement): void {
		const source = new MSE.Source({
			broadcast: this.broadcast,
			latency: this.latency,
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

		effect.proxy(this.audio.catalog, source.audio.catalog);
		effect.proxy(this.audio.rendition, source.audio.rendition);
		effect.proxy(this.audio.config, source.audio.config);
		effect.proxy(this.audio.stats, source.audio.stats);
	}
}

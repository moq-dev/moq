import * as Moq from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import * as Audio from "./audio";
import type { Broadcast } from "./broadcast";
import type { BufferedRanges } from "./buffered";
import { Muxer } from "./mse";
import { type Latency, Sync } from "./sync";
import * as Video from "./video";

export interface Backend {
	// Whether audio/video playback is paused.
	paused: Signal<boolean>;

	// Video specific signals.
	video?: Video.Backend;

	// Audio specific signals.
	audio?: Audio.Backend;

	// The latency target: a scalar minimizes (collapsed range), an object opens a range. See {@link Latency}.
	latency: Signal<Latency>;

	// The jitter buffer in milliseconds.
	jitter: Signal<Moq.Time.Milli>;

	// Derived: whether buffered playback is active (ceiling above floor).
	buffered: Signal<boolean>;

	// Re-anchor playback (and flush the audio buffer) at an utterance boundary in buffered mode.
	reset(): void;
}

export interface MultiBackendProps {
	element?: HTMLCanvasElement | HTMLVideoElement | Signal<HTMLCanvasElement | HTMLVideoElement | undefined>;
	broadcast?: Broadcast | Signal<Broadcast | undefined>;

	// Latency target. A scalar (or "real-time") minimizes; an object `{ min, max }` opens a range and
	// enables buffered playback: build up a buffer from future-dated frames (e.g. TTS) instead of
	// minimizing latency. Call `reset()` at each utterance boundary to re-anchor. See {@link Latency}.
	latency?: Latency | Signal<Latency>;

	// Established connection, used by Sync to read RTT (PROBE) for dynamic jitter in "real-time" mode.
	connection?: Signal<Moq.Connection.Established | undefined>;

	paused?: boolean | Signal<boolean>;

	// When video is downloaded relative to the canvas position. See {@link Video.Visible}.
	visible?: Video.Visible | Signal<Video.Visible>;
}

// We have to proxy some of these signals because we support both the MSE and WebCodecs.
class VideoBackend implements Video.Backend {
	// The source of the video.
	source: Video.Source;

	// The stats of the video.
	stats = new Signal<Video.Stats | undefined>(undefined);

	// We're currently stalled waiting for the next frame
	stalled = new Signal<boolean>(false);

	// Buffered time ranges
	buffered = new Signal<BufferedRanges>([]);

	// The timestamp of the current frame
	timestamp = new Signal<Moq.Time.Milli>(Moq.Time.Milli.zero);

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

	// Buffered time ranges
	buffered = new Signal<BufferedRanges>([]);

	// The AudioContext used for playback (set by the WebCodecs backend; undefined under MSE).
	context = new Signal<AudioContext | undefined>(undefined);

	constructor(source: Audio.Source) {
		this.source = source;
	}
}

/// A generic backend that supports either MSE or WebCodecs based on the provided element.
///
/// This is primarily what backs the <moq-watch> web component, but it's useful as a standalone for other use cases.
export class MultiBackend implements Backend {
	element = new Signal<HTMLCanvasElement | HTMLVideoElement | undefined>(undefined);
	broadcast: Signal<Broadcast | undefined>;
	latency: Signal<Latency>;
	jitter: Signal<Moq.Time.Milli>;
	buffered: Signal<boolean>;
	paused: Signal<boolean>;

	// When video is downloaded relative to the canvas position. See {@link Video.Visible}.
	visible: Signal<Video.Visible>;

	video: VideoBackend;
	#videoSource: Video.Source;

	audio: AudioBackend;
	#audioSource: Audio.Source;

	// The active WebCodecs audio decoder, used to flush the buffer on `reset()`.
	#audioDecoder?: Audio.Decoder;

	// Used to sync audio and video playback at a target delay.
	sync: Sync;

	signals = new Effect();

	constructor(props?: MultiBackendProps) {
		this.element = Signal.from(props?.element);
		this.broadcast = Signal.from(props?.broadcast);
		this.sync = new Sync({
			latency: props?.latency,
			connection: props?.connection,
		});
		this.latency = this.sync.latency;
		this.jitter = this.sync.jitter;
		this.buffered = this.sync.buffered;

		this.#videoSource = new Video.Source(this.sync, {
			broadcast: this.broadcast,
		});
		this.#audioSource = new Audio.Source(this.sync, {
			broadcast: this.broadcast,
		});

		this.video = new VideoBackend(this.#videoSource);
		this.audio = new AudioBackend(this.#audioSource);

		this.paused = Signal.from(props?.paused ?? false);
		this.visible = Signal.from(props?.visible ?? "0px");

		this.signals.run(this.#runElement.bind(this));
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
		this.#audioDecoder = audioSource;

		const audioEmitter = new Audio.Emitter(audioSource, {
			volume: this.audio.volume,
			muted: this.audio.muted,
			paused: this.paused,
		});

		const videoRenderer = new Video.Renderer(videoSource, {
			canvas: element,
			paused: this.paused,
			visible: this.visible,
		});

		effect.cleanup(() => {
			videoSource.close();
			audioSource.close();
			audioEmitter.close();
			videoRenderer.close();
			this.#audioDecoder = undefined;
		});

		// Proxy the read only signals to the backend.
		effect.proxy(this.video.stats, videoSource.stats);
		effect.proxy(this.video.buffered, videoSource.buffered);
		effect.proxy(this.video.stalled, videoSource.stalled);
		effect.proxy(this.video.timestamp, videoSource.timestamp);

		effect.proxy(this.audio.stats, audioSource.stats);
		effect.proxy(this.audio.buffered, audioSource.buffered);
		effect.proxy(this.audio.context, audioSource.context);
	}

	#runMse(effect: Effect, element: HTMLVideoElement): void {
		const mse = new Muxer(this.sync, {
			paused: this.paused,
			element,
		});

		const video = new Video.Mse(mse, this.#videoSource);
		const audio = new Audio.Mse(mse, this.#audioSource, {
			volume: this.audio.volume,
			muted: this.audio.muted,
		});

		effect.cleanup(() => {
			video.close();
			audio.close();
			mse.close();
		});

		// Proxy the read only signals to the backend.
		effect.proxy(this.video.stats, video.stats);
		effect.proxy(this.video.buffered, video.buffered);
		effect.proxy(this.video.stalled, video.stalled);
		effect.proxy(this.video.timestamp, video.timestamp);

		effect.proxy(this.audio.stats, audio.stats);
		effect.proxy(this.audio.buffered, audio.buffered);
		effect.proxy(this.audio.context, audio.context);
	}

	// Re-anchor playback at an utterance boundary in buffered mode: reset the sync reference
	// and flush the audio buffer so the next utterance plays from its own first frame.
	reset(): void {
		this.sync.reset();
		this.#audioDecoder?.reset();
	}

	close(): void {
		this.signals.close();
		this.#videoSource.close();
		this.#audioSource.close();
		this.sync.close();
	}
}

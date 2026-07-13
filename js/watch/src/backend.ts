import * as Moq from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import * as Audio from "./audio";
import type { Broadcast } from "./broadcast";
import type { BufferedRanges } from "./buffered";
import { Muxer } from "./mse";
import { type Latency, Sync } from "./sync";
import * as Video from "./video";

type VideoBackendOutput = {
	stats: Signal<Video.Stats | undefined>;
	stalled: Signal<boolean>;
	buffered: Signal<BufferedRanges>;
	timestamp: Signal<Moq.Time.Milli>;
};

// Unifies the video outputs across the MSE and WebCodecs backends.
class VideoBackend implements Video.Backend {
	source: Video.Source;
	readonly out: Readonlys<VideoBackendOutput>;

	constructor(source: Video.Source, output: VideoBackendOutput) {
		this.source = source;
		this.out = readonlys(output);
	}
}

type AudioBackendOutput = {
	stats: Signal<Audio.Stats | undefined>;
	buffered: Signal<BufferedRanges>;
	context: Signal<AudioContext | undefined>;
};

// Unifies the audio outputs across the MSE and WebCodecs backends.
class AudioBackend implements Audio.Backend {
	source: Audio.Source;
	readonly out: Readonlys<AudioBackendOutput>;

	constructor(source: Audio.Source, output: AudioBackendOutput) {
		this.source = source;
		this.out = readonlys(output);
	}
}

type MultiBackendInput = {
	element: Getter<HTMLCanvasElement | HTMLVideoElement | undefined>;
	broadcast: Getter<Broadcast | undefined>;

	// Established connection, used by Sync to read RTT (PROBE) for dynamic jitter in "real-time" mode.
	connection: Getter<Moq.Connection.Established | undefined>;

	paused: Getter<boolean>;

	// When video is downloaded relative to the canvas position. See {@link Video.Visible}.
	visible: Getter<Video.Visible>;

	// Latency target. A scalar (or "real-time") minimizes; an object `{ min, max }` opens a range and
	// enables buffered playback. Call `reset()` at each utterance boundary to re-anchor. See {@link Latency}.
	latency: Getter<Latency>;

	// Audio controls. The owner holds the writable Signals.
	volume: Getter<number>;
	muted: Getter<boolean>;

	// The desired video rendition (resolution/bitrate cap).
	target: Getter<Video.Target | undefined>;
};

/// A generic backend that supports either MSE or WebCodecs based on the provided element.
///
/// This is primarily what backs the <moq-watch> web component, but it's useful as a standalone for other use cases.
export class MultiBackend {
	readonly in: Readonlys<MultiBackendInput>;

	// Read-only signals computed by Sync: the jitter buffer (ms) and whether buffered playback is active.
	readonly out: { readonly jitter: Getter<Moq.Time.Milli>; readonly buffered: Getter<boolean> };

	#videoSource: Video.Source;
	#audioSource: Audio.Source;

	// The active backend supplies its support probe; the source filters renditions with it.
	#videoSupported = new Signal<Video.Supported | undefined>(undefined);
	#audioSupported = new Signal<Audio.Supported | undefined>(undefined);

	// Whether to download. Driven by the renderer/emitter policy, read by the decoders.
	#videoEnabled = new Signal(false);
	#audioEnabled = new Signal(false);

	#videoOutput: VideoBackendOutput = {
		stats: new Signal<Video.Stats | undefined>(undefined),
		stalled: new Signal<boolean>(false),
		buffered: new Signal<BufferedRanges>([]),
		timestamp: new Signal<Moq.Time.Milli>(Moq.Time.Milli.zero),
	};
	#audioOutput: AudioBackendOutput = {
		stats: new Signal<Audio.Stats | undefined>(undefined),
		buffered: new Signal<BufferedRanges>([]),
		context: new Signal<AudioContext | undefined>(undefined),
	};

	video: VideoBackend;
	audio: AudioBackend;

	// The active WebCodecs audio decoder, used to flush the buffer on `reset()`.
	#audioDecoder?: Audio.Decoder;

	// Used to sync audio and video playback at a target delay.
	sync: Sync;

	signals = new Effect();

	constructor(props?: Inputs<MultiBackendInput>) {
		this.in = {
			element: getter(props?.element),
			broadcast: getter(props?.broadcast),
			connection: getter(props?.connection),
			paused: getter(props?.paused ?? false),
			visible: getter(props?.visible ?? "20%"),
			latency: getter(props?.latency ?? ("real-time" as Latency)),
			volume: getter(props?.volume ?? 0.5),
			muted: getter(props?.muted ?? false),
			target: getter(props?.target),
		};

		this.#videoSource = new Video.Source({
			broadcast: this.in.broadcast,
			target: this.in.target,
			supported: this.#videoSupported,
		});
		this.#audioSource = new Audio.Source({
			broadcast: this.in.broadcast,
			supported: this.#audioSupported,
		});

		// Sources produce the per-rendition jitter that Sync reads, so they're created
		// before Sync to avoid a construction cycle.
		this.sync = new Sync({
			latency: this.in.latency,
			connection: this.in.connection,
			video: this.#videoSource.out.jitter,
			audio: this.#audioSource.out.jitter,
		});

		this.video = new VideoBackend(this.#videoSource, this.#videoOutput);
		this.audio = new AudioBackend(this.#audioSource, this.#audioOutput);

		this.out = { jitter: this.sync.out.jitter, buffered: this.sync.out.buffered };

		this.signals.run(this.#runElement.bind(this));
	}

	#runElement(effect: Effect): void {
		const element = effect.get(this.in.element);
		if (!element) return;

		if (element instanceof HTMLCanvasElement) {
			this.#runWebcodecs(effect, element);
		} else if (element instanceof HTMLVideoElement) {
			this.#runMse(effect, element);
		}
	}

	#runWebcodecs(effect: Effect, element: HTMLCanvasElement): void {
		// This backend's support probes drive rendition selection.
		effect.set(this.#videoSupported, Video.Decoder.supported, undefined);
		effect.set(this.#audioSupported, Audio.Decoder.supported, undefined);

		const videoDecoder = new Video.Decoder(this.#videoSource, this.sync, { enabled: this.#videoEnabled });
		const audioDecoder = new Audio.Decoder(this.#audioSource, this.sync, { enabled: this.#audioEnabled });
		this.#audioDecoder = audioDecoder;

		const audioEmitter = new Audio.Emitter(audioDecoder, {
			volume: this.in.volume,
			muted: this.in.muted,
			paused: this.in.paused,
		});

		const videoRenderer = new Video.Renderer(videoDecoder, {
			canvas: element,
			visible: this.in.visible,
		});

		effect.cleanup(() => {
			videoDecoder.close();
			audioDecoder.close();
			audioEmitter.close();
			videoRenderer.close();
			this.#audioDecoder = undefined;
		});

		// Audio download follows the emitter's enable policy (paused/muted).
		effect.proxy(this.#audioEnabled, audioEmitter.out.enabled);

		// Video downloads while playing and on-screen. When paused, keep downloading only
		// until a frame is on the canvas, then stop: a cold paused start still shows a poster
		// instead of black, without streaming while paused. Read the rendered frame only in
		// the paused branch so playback doesn't re-run this every painted frame.
		effect.run((inner) => {
			const visible = inner.get(videoRenderer.out.visible);
			if (!inner.get(this.in.paused)) {
				this.#videoEnabled.set(visible);
				return;
			}
			const frame = inner.get(videoRenderer.out.frame);
			this.#videoEnabled.set(visible && !frame);
		});
		effect.cleanup(() => {
			this.#videoEnabled.set(false);
			this.#audioEnabled.set(false);
		});

		// Proxy the read only signals to the backend.
		effect.proxy(this.#videoOutput.stats, videoDecoder.out.stats);
		effect.proxy(this.#videoOutput.buffered, videoDecoder.out.buffered);
		effect.proxy(this.#videoOutput.stalled, videoDecoder.out.stalled);
		effect.proxy(this.#videoOutput.timestamp, videoDecoder.out.timestamp);

		effect.proxy(this.#audioOutput.stats, audioDecoder.out.stats);
		effect.proxy(this.#audioOutput.buffered, audioDecoder.out.buffered);
		effect.proxy(this.#audioOutput.context, audioDecoder.out.context);
	}

	#runMse(effect: Effect, element: HTMLVideoElement): void {
		// This backend's support probes drive rendition selection.
		effect.set(this.#videoSupported, Video.Mse.supported, undefined);
		effect.set(this.#audioSupported, Audio.Mse.supported, undefined);

		const muxer = new Muxer(this.sync, {
			paused: this.in.paused,
			element,
		});

		const video = new Video.Mse(muxer, this.sync, this.#videoSource);
		const audio = new Audio.Mse(muxer, this.sync, this.#audioSource, {
			volume: this.in.volume,
			muted: this.in.muted,
		});

		effect.cleanup(() => {
			video.close();
			audio.close();
			muxer.close();
		});

		// Proxy the read only signals to the backend.
		effect.proxy(this.#videoOutput.stats, video.out.stats);
		effect.proxy(this.#videoOutput.buffered, video.out.buffered);
		effect.proxy(this.#videoOutput.stalled, video.out.stalled);
		effect.proxy(this.#videoOutput.timestamp, video.out.timestamp);

		effect.proxy(this.#audioOutput.stats, audio.out.stats);
		effect.proxy(this.#audioOutput.buffered, audio.out.buffered);
		effect.proxy(this.#audioOutput.context, audio.out.context);
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

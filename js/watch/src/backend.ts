import * as Moq from "@moq/net";
import { Effect, type Getter, getter, type InputProps, type Readonlys, readonlys, Signal } from "@moq/signals";
import * as Audio from "./audio";
import { decoderSupported as audioDecoderSupported } from "./audio/decoder";
import { mseSupported as audioMseSupported } from "./audio/mse";
import type { Broadcast } from "./broadcast";
import { Muxer } from "./mse";
import { type Latency, Sync } from "./sync";
import * as Video from "./video";
import { decoderSupported as videoDecoderSupported } from "./video/decoder";
import { mseSupported as videoMseSupported } from "./video/mse";

// Serializable representation of TimeRanges
export interface BufferedRange {
	start: Moq.Time.Milli;
	end: Moq.Time.Milli;
}
export type BufferedRanges = BufferedRange[];

// Helper to convert DOM TimeRanges
export function timeRangesToArray(ranges: TimeRanges): BufferedRanges {
	const result: BufferedRange[] = [];

	for (let i = 0; i < ranges.length; i++) {
		const start = Moq.Time.Milli.fromSecond(ranges.start(i) as Moq.Time.Second);
		const end = Moq.Time.Milli.fromSecond(ranges.end(i) as Moq.Time.Second);

		result.push({ start, end });
	}
	return result;
}

type VideoBackendOutput = {
	stats: Signal<Video.Stats | undefined>;
	stalled: Signal<boolean>;
	buffered: Signal<BufferedRanges>;
	timestamp: Signal<Moq.Time.Milli>;
};

// Unifies the video outputs across the MSE and WebCodecs backends.
class VideoBackend implements Video.Backend {
	source: Video.Source;
	readonly output: Readonlys<VideoBackendOutput>;

	constructor(source: Video.Source, output: VideoBackendOutput) {
		this.source = source;
		this.output = readonlys(output);
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
	readonly output: Readonlys<AudioBackendOutput>;

	constructor(source: Audio.Source, output: AudioBackendOutput) {
		this.source = source;
		this.output = readonlys(output);
	}
}

type MultiBackendInput = {
	element: Getter<HTMLCanvasElement | HTMLVideoElement | undefined>;
	broadcast: Getter<Broadcast | undefined>;

	// Established connection, used by Sync to read RTT (PROBE) for dynamic jitter in "real-time" mode.
	connection: Getter<Moq.Connection.Established | undefined>;

	paused: Getter<boolean>;

	// Latency: "real-time" auto-computes jitter from RTT, a number sets a fixed jitter in ms.
	latency: Getter<Latency>;

	// Audio controls. The owner holds the writable Signals.
	volume: Getter<number>;
	muted: Getter<boolean>;

	// The desired video rendition (resolution/bitrate cap).
	target: Getter<Video.Target | undefined>;
};

export type MultiBackendProps = InputProps<MultiBackendInput>;

/// A generic backend that supports either MSE or WebCodecs based on the provided element.
///
/// This is primarily what backs the <moq-watch> web component, but it's useful as a standalone for other use cases.
export class MultiBackend {
	readonly input: Readonlys<MultiBackendInput>;

	// The jitter buffer in milliseconds, computed by Sync.
	readonly output: { readonly jitter: Getter<Moq.Time.Milli> };

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

	// Used to sync audio and video playback at a target delay.
	sync: Sync;

	signals = new Effect();

	constructor(props?: MultiBackendProps) {
		this.input = {
			element: getter(props?.element),
			broadcast: getter(props?.broadcast),
			connection: getter(props?.connection),
			paused: getter(props?.paused ?? false),
			latency: getter(props?.latency ?? ("real-time" as Latency)),
			volume: getter(props?.volume ?? 0.5),
			muted: getter(props?.muted ?? false),
			target: getter(props?.target),
		};

		this.#videoSource = new Video.Source({
			broadcast: this.input.broadcast,
			target: this.input.target,
			supported: this.#videoSupported,
		});
		this.#audioSource = new Audio.Source({
			broadcast: this.input.broadcast,
			supported: this.#audioSupported,
		});

		// Sources produce their per-rendition jitter, which Sync consumes. Sources
		// don't depend on Sync, so this ordering avoids a construction cycle.
		this.sync = new Sync({
			latency: this.input.latency,
			connection: this.input.connection,
			video: this.#videoSource.output.jitter,
			audio: this.#audioSource.output.jitter,
		});

		this.video = new VideoBackend(this.#videoSource, this.#videoOutput);
		this.audio = new AudioBackend(this.#audioSource, this.#audioOutput);

		this.output = { jitter: this.sync.output.jitter };

		this.signals.run(this.#runElement.bind(this));
	}

	#runElement(effect: Effect): void {
		const element = effect.get(this.input.element);
		if (!element) return;

		if (element instanceof HTMLCanvasElement) {
			this.#runWebcodecs(effect, element);
		} else if (element instanceof HTMLVideoElement) {
			this.#runMse(effect, element);
		}
	}

	#runWebcodecs(effect: Effect, element: HTMLCanvasElement): void {
		// This backend's support probes drive rendition selection.
		effect.set(this.#videoSupported, videoDecoderSupported, undefined);
		effect.set(this.#audioSupported, audioDecoderSupported, undefined);

		const videoDecoder = new Video.Decoder(this.#videoSource, this.sync, { enabled: this.#videoEnabled });
		const audioDecoder = new Audio.Decoder(this.#audioSource, this.sync, { enabled: this.#audioEnabled });

		const audioEmitter = new Audio.Emitter(audioDecoder, {
			volume: this.input.volume,
			muted: this.input.muted,
			paused: this.input.paused,
		});

		const videoRenderer = new Video.Renderer(videoDecoder, {
			canvas: element,
		});

		effect.cleanup(() => {
			videoDecoder.close();
			audioDecoder.close();
			audioEmitter.close();
			videoRenderer.close();
		});

		// Audio download follows the emitter's enable policy (paused/muted).
		effect.proxy(this.#audioEnabled, audioEmitter.output.enabled);

		// Video download policy (relocated from the renderer): when playing, follow
		// visibility; when paused, fetch a single preview frame then stop.
		effect.run((inner) => {
			const paused = inner.get(this.input.paused);
			const visible = inner.get(videoRenderer.output.visible);
			if (!paused) {
				this.#videoEnabled.set(visible);
				return;
			}
			const frame = inner.get(videoDecoder.output.frame);
			this.#videoEnabled.set(!frame);
		});
		effect.cleanup(() => {
			this.#videoEnabled.set(false);
			this.#audioEnabled.set(false);
		});

		// Proxy the read only signals to the backend.
		effect.proxy(this.#videoOutput.stats, videoDecoder.output.stats);
		effect.proxy(this.#videoOutput.buffered, videoDecoder.output.buffered);
		effect.proxy(this.#videoOutput.stalled, videoDecoder.output.stalled);
		effect.proxy(this.#videoOutput.timestamp, videoDecoder.output.timestamp);

		effect.proxy(this.#audioOutput.stats, audioDecoder.output.stats);
		effect.proxy(this.#audioOutput.buffered, audioDecoder.output.buffered);
		effect.proxy(this.#audioOutput.context, audioDecoder.output.context);
	}

	#runMse(effect: Effect, element: HTMLVideoElement): void {
		// This backend's support probes drive rendition selection.
		effect.set(this.#videoSupported, videoMseSupported, undefined);
		effect.set(this.#audioSupported, audioMseSupported, undefined);

		const muxer = new Muxer(this.sync, {
			paused: this.input.paused,
			element,
		});

		const video = new Video.Mse(muxer, this.sync, this.#videoSource);
		const audio = new Audio.Mse(muxer, this.sync, this.#audioSource, {
			volume: this.input.volume,
			muted: this.input.muted,
		});

		effect.cleanup(() => {
			video.close();
			audio.close();
			muxer.close();
		});

		// Proxy the read only signals to the backend.
		effect.proxy(this.#videoOutput.stats, video.output.stats);
		effect.proxy(this.#videoOutput.buffered, video.output.buffered);
		effect.proxy(this.#videoOutput.stalled, video.output.stalled);
		effect.proxy(this.#videoOutput.timestamp, video.output.timestamp);

		effect.proxy(this.#audioOutput.stats, audio.output.stats);
		effect.proxy(this.#audioOutput.buffered, audio.output.buffered);
		effect.proxy(this.#audioOutput.context, audio.output.context);
	}

	close(): void {
		this.signals.close();
		this.#videoSource.close();
		this.#audioSource.close();
		this.sync.close();
	}
}

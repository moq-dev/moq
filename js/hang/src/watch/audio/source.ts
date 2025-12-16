import type * as Moq from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import type * as Catalog from "../../catalog";
import * as Frame from "../../frame";
import type * as Time from "../../time";
import * as Hex from "../../util/hex";
import * as libav from "../../util/libav";
import type * as Render from "./render";

// We want some extra overhead to avoid starving the render worklet.
// The default Opus frame duration is 20ms.
// TODO: Put it in the catalog so we don't have to guess.
const JITTER_UNDERHEAD = 25 as Time.Milli;

export type SourceProps = {
	// Enable to download the audio track.
	enabled?: boolean | Signal<boolean>;

	// Jitter buffer size in milliseconds (default: 100ms)
	latency?: Time.Milli | Signal<Time.Milli>;
};

export interface AudioStats {
	bytesReceived: number;
}

// Unfortunately, we need to use a Vite-exclusive import for now.
import RenderWorklet from "./render-worklet.ts?worker&url";

// Downloads audio from a track and emits it to an AudioContext.
// The user is responsible for hooking up audio to speakers, an analyzer, etc.
export class Source {
	broadcast: Getter<Moq.Broadcast | undefined>;
	enabled: Signal<boolean>;

	#context = new Signal<AudioContext | undefined>(undefined);
	readonly context: Getter<AudioContext | undefined> = this.#context;

	// The root of the audio graph, which can be used for custom visualizations.
	#worklet = new Signal<AudioWorkletNode | undefined>(undefined);
	// Downcast to AudioNode so it matches Publish.Audio
	readonly root = this.#worklet as Getter<AudioNode | undefined>;

	#sampleRate = new Signal<number | undefined>(undefined);
	readonly sampleRate: Getter<number | undefined> = this.#sampleRate;

	#stats = new Signal<AudioStats | undefined>(undefined);
	readonly stats: Getter<AudioStats | undefined> = this.#stats;

	catalog = new Signal<Catalog.Audio | undefined>(undefined);
	config = new Signal<Catalog.AudioConfig | undefined>(undefined);

	// Not a signal because I'm lazy.
	readonly latency: Signal<Time.Milli>;

	// The name of the active rendition.
	active = new Signal<string | undefined>(undefined);

	#signals = new Effect();

	constructor(
		broadcast: Getter<Moq.Broadcast | undefined>,
		catalog: Getter<Catalog.Root | undefined>,
		props?: SourceProps,
	) {
		this.broadcast = broadcast;
		this.enabled = Signal.from(props?.enabled ?? false);
		this.latency = Signal.from(props?.latency ?? (100 as Time.Milli)); // TODO Reduce this once fMP4 stuttering is fixed.

		this.#signals.effect((effect) => {
			const audio = effect.get(catalog)?.audio;
			this.catalog.set(audio);

			if (audio?.renditions) {
				const first = Object.entries(audio.renditions).at(0);
				if (first) {
					effect.set(this.active, first[0]);
					effect.set(this.config, first[1]);
				}
			}
		});

		this.#signals.effect(this.#runWorklet.bind(this));
		this.#signals.effect(this.#runEnabled.bind(this));
		this.#signals.effect(this.#runDecoder.bind(this));
	}

	#runWorklet(effect: Effect): void {
		// It takes a second or so to initialize the AudioContext/AudioWorklet, so do it even if disabled.
		// This is less efficient for video-only playback but makes muting/unmuting instant.

		//const enabled = effect.get(this.enabled);
		//if (!enabled) return;

		const config = effect.get(this.config);
		if (!config) return;

		const sampleRate = config.sampleRate;
		const channelCount = config.numberOfChannels;

		// NOTE: We still create an AudioContext even when muted.
		// This way we can process the audio for visualizations.

		const context = new AudioContext({
			latencyHint: "interactive", // We don't use real-time because of the jitter buffer.
			sampleRate,
		});
		effect.set(this.#context, context);

		effect.cleanup(() => context.close());

		effect.spawn(async () => {
			// Register the AudioWorklet processor
			await context.audioWorklet.addModule(RenderWorklet);

			// Ensure the context is running before creating the worklet
			if (context.state === "closed") return;

			// Create the worklet node
			const worklet = new AudioWorkletNode(context, "render", {
				channelCount,
				channelCountMode: "explicit",
			});
			effect.cleanup(() => worklet.disconnect());

			const init: Render.Init = {
				type: "init",
				rate: sampleRate,
				channels: channelCount,
				latency: this.latency.peek(), // TODO make it reactive
			};
			worklet.port.postMessage(init);

			effect.set(this.#worklet, worklet);
		});
	}

	#runEnabled(effect: Effect): void {
		const enabled = effect.get(this.enabled);
		if (!enabled) return;

		const context = effect.get(this.#context);
		if (!context) return;

		context.resume();

		// NOTE: You should disconnect/reconnect the worklet to save power when disabled.
	}

	#runDecoder(effect: Effect): void {
		const enabled = effect.get(this.enabled);
		if (!enabled) return;

		const catalog = effect.get(this.catalog);
		if (!catalog) return;

		const broadcast = effect.get(this.broadcast);
		if (!broadcast) return;

		const config = effect.get(this.config);
		if (!config) return;

		const active = effect.get(this.active);
		if (!active) return;

		const sub = broadcast.subscribe(active, catalog.priority);
		effect.cleanup(() => sub.close());

		// Create consumer with slightly less latency than the render worklet to avoid underflowing.
		const consumer = new Frame.Consumer(sub, {
			latency: Math.max(this.latency.peek() - JITTER_UNDERHEAD, 0) as Time.Milli,
		});
		effect.cleanup(() => consumer.close());

		effect.spawn(async () => {
			const loaded = await libav.polyfill();
			if (!loaded) return; // cancelled

			const decoder = new AudioDecoder({
				output: (data) => this.#emit(data),
				error: (error) => console.error(error),
			});
			effect.cleanup(() => decoder.close());

			const description = config.description ? Hex.toBytes(config.description) : undefined;
			decoder.configure({
				...config,
				description,
			});

			for (;;) {
				const frame = await consumer.decode();
				if (!frame) break;

				this.#stats.update((stats) => ({
					bytesReceived: (stats?.bytesReceived ?? 0) + frame.data.byteLength,
				}));

				const chunk = new EncodedAudioChunk({
					type: frame.keyframe ? "key" : "delta",
					data: frame.data,
					timestamp: frame.timestamp,
				});

				decoder.decode(chunk);
			}
		});
	}

	#emit(sample: AudioData) {
		const timestamp = sample.timestamp as Time.Micro;

		const worklet = this.#worklet.peek();
		if (!worklet) {
			// We're probably in the process of closing.
			sample.close();
			return;
		}

		const channelData: Float32Array[] = [];
		for (let channel = 0; channel < sample.numberOfChannels; channel++) {
			const data = new Float32Array(sample.numberOfFrames);
			sample.copyTo(data, { format: "f32-planar", planeIndex: channel });
			channelData.push(data);
		}

		const msg: Render.Data = {
			type: "data",
			data: channelData,
			timestamp,
		};

		// Send audio data to worklet via postMessage
		// TODO: At some point, use SharedArrayBuffer to avoid dropping samples.
		worklet.port.postMessage(
			msg,
			msg.data.map((data) => data.buffer),
		);

		sample.close();
	}

	close() {
		this.#signals.close();
	}
}

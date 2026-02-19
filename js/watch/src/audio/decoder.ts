import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Util from "@moq/hang/util";
import type * as Moq from "@moq/lite";
import { Time } from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import type { BufferedRanges } from "../backend";
import type * as Render from "./render";
import type { ToMain } from "./render";
// Compiled and inlined as a blob URL via vite-plugin-worklet.
import RenderWorklet from "./render-worklet.ts?worklet";
import type { Source } from "./source";

export type DecoderProps = {
	// Enable to download the audio track.
	enabled?: boolean | Signal<boolean>;
};

export interface AudioStats {
	bytesReceived: number;
}

// Downloads audio from a track and emits it to an AudioContext.
// The user is responsible for hooking up audio to speakers, an analyzer, etc.
export class Decoder {
	source: Source;
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

	// Current playback timestamp from worklet
	#timestamp = new Signal<Time.Milli | undefined>(undefined);
	readonly timestamp: Getter<Time.Milli | undefined> = this.#timestamp;

	// Whether the audio buffer is stalled (waiting to fill)
	#stalled = new Signal<boolean>(true);
	readonly stalled: Getter<boolean> = this.#stalled;

	// Decode buffer: audio sent to worklet but not yet played
	#decodeBuffered = new Signal<BufferedRanges>([]);

	// Combined buffered ranges (network jitter + decode buffer)
	#buffered = new Signal<BufferedRanges>([]);
	readonly buffered: Getter<BufferedRanges> = this.#buffered;

	#signals = new Effect();

	constructor(source: Source, props?: DecoderProps) {
		this.source = source;
		this.source.supported.set(supported); // super hacky

		this.enabled = Signal.from(props?.enabled ?? false);

		this.#signals.run(this.#runWorklet.bind(this));
		this.#signals.run(this.#runEnabled.bind(this));
		this.#signals.run(this.#runLatency.bind(this));
		this.#signals.run(this.#runDecoder.bind(this));
	}

	#runWorklet(effect: Effect): void {
		// It takes a second or so to initialize the AudioContext/AudioWorklet, so do it even if disabled.
		// This is less efficient for video-only playback but makes muting/unmuting instant.

		//const enabled = effect.get(this.enabled);
		//if (!enabled) return;

		const config = effect.get(this.source.config);
		if (!config) return;

		const sampleRate = config.sampleRate;
		const channelCount = config.numberOfChannels;

		// NOTE: We still create an AudioContext even when muted.
		// This way we can process the audio for visualizations.

		const context = new AudioContext({
			latencyHint: "interactive", // We don't use real-time because of the buffer.
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
				latency: this.source.sync.latency.peek(), // Updated reactively via #runLatency
			};
			worklet.port.postMessage(init);

			// Listen for state updates from worklet
			worklet.port.onmessage = (event: MessageEvent<ToMain>) => {
				if (event.data.type === "state") {
					const timestamp = Time.Milli.fromMicro(event.data.timestamp);
					this.#timestamp.set(timestamp);
					this.#stalled.set(event.data.stalled);
					this.#trimDecodeBuffered(timestamp);
				}
			};

			effect.set(this.#worklet, worklet);
		});
	}

	#runEnabled(effect: Effect): void {
		const values = effect.getAll([this.enabled, this.#context]);
		if (!values) return;
		const [_, context] = values;

		context.resume();

		// NOTE: You should disconnect/reconnect the worklet to save power when disabled.
	}

	#runLatency(effect: Effect): void {
		const worklet = effect.get(this.#worklet);
		if (!worklet) return;

		const latency = effect.get(this.source.sync.latency);

		const msg: Render.Latency = {
			type: "latency",
			latency,
		};
		worklet.port.postMessage(msg);
	}

	#runDecoder(effect: Effect): void {
		const enabled = effect.get(this.enabled);
		if (!enabled) return;

		const broadcast = effect.get(this.source.broadcast);
		if (!broadcast) return;

		const track = effect.get(this.source.track);
		if (!track) return;

		const config = effect.get(this.source.config);
		if (!config) return;

		const active = effect.get(broadcast.active);
		if (!active) return;

		const sub = active.subscribe(track, Catalog.PRIORITY.audio);
		effect.cleanup(() => sub.close());

		if (config.container.kind === "cmaf") {
			this.#runCmafDecoder(effect, sub, config);
		} else {
			this.#runLegacyDecoder(effect, sub, config);
		}
	}

	#runLegacyDecoder(effect: Effect, sub: Moq.Track, config: Catalog.AudioConfig): void {
		// Create consumer with slightly less latency than the render worklet to avoid underflowing.
		// TODO include JITTER_UNDERHEAD
		const consumer = new Container.Legacy.Consumer(sub, {
			latency: this.source.sync.latency,
		});
		effect.cleanup(() => consumer.close());

		// Combine network jitter buffer with decode buffer
		effect.run((inner) => {
			const network = inner.get(consumer.buffered);
			const decode = inner.get(this.#decodeBuffered);
			this.#buffered.update(() => mergeBufferedRanges(network, decode));
		});

		effect.spawn(async () => {
			const loaded = await Util.Libav.polyfill();
			if (!loaded) return; // cancelled

			const decoder = new AudioDecoder({
				output: (data) => this.#emit(data),
				error: (error) => console.error(error),
			});
			effect.cleanup(() => decoder.close());

			const description = config.description ? Util.Hex.toBytes(config.description) : undefined;
			decoder.configure({
				...config,
				description,
			});

			for (;;) {
				const next = await consumer.next();
				if (!next) break;

				const { frame } = next;
				if (!frame) continue; // Skip over group done notifications.

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

	#runCmafDecoder(effect: Effect, sub: Moq.Track, config: Catalog.AudioConfig): void {
		if (config.container.kind !== "cmaf") return; // just to help typescript

		const { timescale } = config.container;
		const description = config.description ? Util.Hex.toBytes(config.description) : undefined;

		// For CMAF, just use decode buffer (no network jitter buffer yet)
		// TODO: Add CMAF consumer wrapper for latency control
		effect.run((inner) => {
			const decode = inner.get(this.#decodeBuffered);
			this.#buffered.update(() => decode);
		});

		effect.spawn(async () => {
			const loaded = await Util.Libav.polyfill();
			if (!loaded) return; // cancelled

			const decoder = new AudioDecoder({
				output: (data) => this.#emit(data),
				error: (error) => console.error(error),
			});
			effect.cleanup(() => decoder.close());

			// Configure decoder with description from catalog
			decoder.configure({
				codec: config.codec,
				sampleRate: config.sampleRate,
				numberOfChannels: config.numberOfChannels,
				description,
			});

			// Process data segments
			// TODO: Use a consumer wrapper for CMAF to support latency control
			for (;;) {
				const group = await sub.nextGroup();
				if (!group) break;

				effect.spawn(async () => {
					try {
						for (;;) {
							const segment = await group.readFrame();
							if (!segment) break;

							const samples = Container.Cmaf.decodeDataSegment(segment, timescale);

							for (const sample of samples) {
								this.#stats.update((stats) => ({
									bytesReceived: (stats?.bytesReceived ?? 0) + sample.data.byteLength,
								}));

								const chunk = new EncodedAudioChunk({
									type: sample.keyframe ? "key" : "delta",
									data: sample.data,
									timestamp: sample.timestamp,
								});

								decoder.decode(chunk);
							}
						}
					} finally {
						group.close();
					}
				});
			}
		});
	}

	#emit(sample: AudioData) {
		const timestamp = sample.timestamp as Time.Micro;
		const timestampMilli = Time.Milli.fromMicro(timestamp);

		const worklet = this.#worklet.peek();
		if (!worklet) {
			// We're probably in the process of closing.
			sample.close();
			return;
		}

		// Calculate end time from sample duration
		const durationMicro = ((sample.numberOfFrames / sample.sampleRate) * 1_000_000) as Time.Micro;
		const durationMilli = Time.Milli.fromMicro(durationMicro);
		const end = Time.Milli.add(timestampMilli, durationMilli);

		// Add to decode buffer
		this.#addDecodeBuffered(timestampMilli, end);

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

	#addDecodeBuffered(start: Time.Milli, end: Time.Milli): void {
		if (start > end) return;

		this.#decodeBuffered.mutate((current) => {
			for (const range of current) {
				// Extend range if new sample overlaps or is adjacent (1ms tolerance for float precision)
				if (start <= range.end + 1 && end >= range.start) {
					range.start = Time.Milli.min(range.start, start);
					range.end = Time.Milli.max(range.end, end);
					return;
				}
			}

			current.push({ start, end });
			current.sort((a, b) => a.start - b.start);
		});
	}

	#trimDecodeBuffered(timestamp: Time.Milli): void {
		this.#decodeBuffered.mutate((current) => {
			while (current.length > 0) {
				if (current[0].end >= timestamp) {
					current[0].start = Time.Milli.max(current[0].start, timestamp);
					break;
				}
				current.shift();
			}
		});
	}

	close() {
		this.#signals.close();
	}
}

async function supported(config: Catalog.AudioConfig): Promise<boolean> {
	const description = config.description ? Util.Hex.toBytes(config.description) : undefined;
	const res = await AudioDecoder.isConfigSupported({
		...config,
		description,
	});
	return res.supported ?? false;
}

// Merge two sets of buffered ranges into one sorted list
function mergeBufferedRanges(a: BufferedRanges, b: BufferedRanges): BufferedRanges {
	if (a.length === 0) return b;
	if (b.length === 0) return a;

	const result: BufferedRanges = [];
	const all = [...a, ...b].sort((x, y) => x.start - y.start);

	for (const range of all) {
		const last = result.at(-1);
		if (last && last.end >= range.start) {
			// Merge overlapping ranges
			last.end = Time.Milli.max(last.end, range.end);
		} else {
			result.push({ ...range });
		}
	}

	return result;
}

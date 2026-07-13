import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Util from "@moq/hang/util";
import type * as Moq from "@moq/net";
import { Time } from "@moq/net";
import { Effect, type Getter, Signal } from "@moq/signals";
import { base64ToBytes } from "../base64";
import type { BufferedRanges } from "../buffered";
import { type AudioBuffer, createAudioBuffer } from "./buffer";
// Compiled and inlined as a blob URL via vite-plugin-worklet.
import RenderWorklet from "./render-worklet.ts?worklet";
import type { Source } from "./source";

export type DecoderProps = {
	// Enable to download the audio track.
	enabled?: boolean | Signal<boolean>;
};

export interface AudioStats {
	/** Number of encoded bytes received. */
	bytesReceived: number;
}

/**
 * Downloads audio from a track and emits it to an AudioContext.
 *
 * The user is responsible for hooking up audio to speakers, an analyzer, etc.
 */
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

	// Audio ring bridging main thread and worklet (shared memory or postMessage transport).
	#ring: AudioBuffer | undefined;

	// The last discontinuity count seen from the container consumer. A change means the
	// publisher rewound the timeline (e.g. a voice agent interrupted) and we must flush.
	#discontinuity = 0;

	#signals = new Effect();

	// How much buffered audio the container consumer retains before skipping
	// ahead. This must be the latency CEILING (maxBuffer), not the floor
	// (buffer): in buffered playback the producer writes faster than real-time
	// with future PTS, so the group span legitimately exceeds the floor and
	// would otherwise be skipped. When collapsed, maxBuffer equals the floor.
	//
	// Held in a plain Signal driven by a running effect (below) rather than a
	// lazy `computed`: the container consumer only `.peek()`s this (it never
	// subscribes), and an unsubscribed computed peeks as `undefined`, which
	// would make the consumer's threshold NaN and skip every group.
	#consumerLatency = new Signal<Time.Milli>(Time.Milli.zero);

	constructor(source: Source, props?: DecoderProps) {
		this.source = source;
		this.source.supported.set(supported); // super hacky

		this.enabled = Signal.from(props?.enabled ?? false);

		this.#signals.run((effect) => {
			this.#consumerLatency.set(effect.get(this.source.sync.maxBuffer));
		});

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
			await Promise.race([context.audioWorklet.addModule(RenderWorklet), effect.cancel]);

			// Ensure the context is running before creating the worklet
			if (context.state === "closed") return;

			// Create the worklet node. outputChannelCount must be set explicitly
			// so the process() callback receives a matching channel layout —
			// Firefox defaults differently than Chrome otherwise.
			const worklet = new AudioWorkletNode(context, "render", {
				channelCount,
				channelCountMode: "explicit",
				outputChannelCount: [channelCount],
			});
			effect.cleanup(() => worklet.disconnect());

			// Initial target latency in samples.
			const latency = this.source.sync.buffer.peek();
			const latencySamples = Math.ceil(sampleRate * Time.Second.fromMilli(latency));
			const buffered = this.source.sync.buffered.peek();

			// Let the factory pick the best transport (SharedArrayBuffer or postMessage).
			const ring = createAudioBuffer(worklet, channelCount, sampleRate, latencySamples, buffered);
			this.#ring = ring;
			effect.cleanup(() => {
				ring.close();
				this.#ring = undefined;
			});

			// Mirror ring state (timestamp/stalled) onto our public signals.
			effect.run((inner) => {
				const ts = Time.Milli.fromMicro(inner.get(ring.timestamp));
				this.#timestamp.set(ts);
				this.#trimDecodeBuffered(ts);
			});
			effect.run((inner) => {
				this.#stalled.set(inner.get(ring.stalled));
			});

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
		// Gate on the worklet signal so this effect re-runs once the ring is created.
		const worklet = effect.get(this.#worklet);
		if (!worklet) return;

		const ring = this.#ring;
		if (!ring) return;

		const latency = effect.get(this.source.sync.buffer);
		const latencySamples = Math.ceil(ring.rate * Time.Second.fromMilli(latency));
		ring.setLatency(latencySamples);
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
		const format = config.container.kind === "loc" ? new Container.Loc.Format() : new Container.Legacy.Format();
		// Create consumer with slightly less latency than the render worklet to avoid underflowing.
		// TODO include JITTER_UNDERHEAD
		const consumer = new Container.Consumer(sub, {
			format,
			latency: this.#consumerLatency,
		});
		effect.cleanup(() => consumer.close());

		// Combine network jitter buffer with decode buffer
		effect.run((inner) => {
			const network = inner.get(consumer.buffered);
			const decode = inner.get(this.#decodeBuffered);
			this.#buffered.update(() => Container.mergeBufferedRanges(network, decode));
		});

		effect.spawn(async () => {
			const loaded = await Util.Libav.polyfill();
			if (!loaded) return; // cancelled

			let warmed = 0;

			const decoder = new AudioDecoder({
				output: (data) => {
					warmed++;
					if (warmed <= 3) {
						// Drop the first 3 frames to prime the decoder.
						data.close();
						return;
					}
					this.#emit(data);
				},
				error: (error) => console.error(error),
			});
			effect.cleanup(() => {
				if (decoder.state !== "closed") decoder.close();
			});

			// Opus in CMAF uses raw packets; dOps is not a valid OGG Identification Header.
			const description =
				config.codec === "opus"
					? undefined
					: config.description
						? Util.Hex.toBytes(config.description)
						: undefined;
			decoder.configure({
				...config,
				description,
			});

			for (;;) {
				const next = await consumer.next();
				if (!next) break;

				// Publisher rewound the timeline: flush + re-anchor before decoding the new frame.
				this.#onDiscontinuity(next.discontinuity);

				const { frame } = next;
				if (!frame) continue;

				// Mark that we received this frame right now.
				const timestamp = Time.Milli.fromMicro(frame.timestamp as Time.Micro);
				this.source.sync.received(timestamp, "audio");

				this.#stats.update((stats) => ({
					bytesReceived: (stats?.bytesReceived ?? 0) + frame.data.byteLength,
				}));

				// Backpressure: in buffered mode this holds the encoded frame until the playhead nears
				// it, keeping the lookahead above the floor as Opus instead of decoded PCM. No-op live.
				await this.#ring?.wait(frame.timestamp as Time.Micro);

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

		const initSegment = base64ToBytes(config.container.init);
		const init = Container.Cmaf.decodeInitSegment(initSegment);
		// Opus in CMAF uses raw packets (not OGG-wrapped), so description must be omitted.
		// The dOps box from the init segment is not a valid OGG Identification Header.
		const description =
			config.codec === "opus"
				? undefined
				: config.description
					? Util.Hex.toBytes(config.description)
					: init.description;

		const consumer = new Container.Consumer(sub, {
			format: new Container.Cmaf.Format(init),
			latency: this.#consumerLatency,
		});
		effect.cleanup(() => consumer.close());

		// Combine network jitter buffer with decode buffer
		effect.run((inner) => {
			const network = inner.get(consumer.buffered);
			const decode = inner.get(this.#decodeBuffered);
			this.#buffered.update(() => Container.mergeBufferedRanges(network, decode));
		});

		effect.spawn(async () => {
			const loaded = await Util.Libav.polyfill();
			if (!loaded) return; // cancelled

			const decoder = new AudioDecoder({
				output: (data) => this.#emit(data),
				error: (error) => console.error(error),
			});
			effect.cleanup(() => {
				if (decoder.state !== "closed") decoder.close();
			});

			// Configure decoder with description from catalog
			decoder.configure({
				codec: config.codec,
				sampleRate: config.sampleRate,
				numberOfChannels: config.numberOfChannels,
				description,
			});

			for (;;) {
				const next = await consumer.next();
				if (!next) break;

				// Publisher rewound the timeline: flush + re-anchor before decoding the new frame.
				this.#onDiscontinuity(next.discontinuity);

				const { frame } = next;
				if (!frame) continue;

				const timestamp = Time.Milli.fromMicro(frame.timestamp);
				this.source.sync.received(timestamp, "audio");

				this.#stats.update((stats) => ({
					bytesReceived: (stats?.bytesReceived ?? 0) + frame.data.byteLength,
				}));

				// Backpressure: in buffered mode this holds the encoded frame until the playhead nears
				// it, keeping the lookahead above the floor as Opus instead of decoded PCM. No-op live.
				await this.#ring?.wait(frame.timestamp);

				if (decoder.state === "closed") break;
				decoder.decode(
					new EncodedAudioChunk({
						type: frame.keyframe ? "key" : "delta",
						data: frame.data,
						timestamp: frame.timestamp,
					}),
				);
			}
		});
	}

	#emit(sample: AudioData) {
		const timestamp = sample.timestamp as Time.Micro;
		const timestampMilli = Time.Milli.fromMicro(timestamp);

		const ring = this.#ring;
		if (!ring) {
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

		// Firefox's Opus decoder sometimes outputs more channels than requested
		// (e.g. 6 for stereo). Clamp to the ring's channel count.
		const channels = Math.min(sample.numberOfChannels, ring.channels);
		const channelData: Float32Array[] = [];
		for (let channel = 0; channel < channels; channel++) {
			const data = new Float32Array(sample.numberOfFrames);
			sample.copyTo(data, { format: "f32-planar", planeIndex: channel });
			channelData.push(data);
		}

		// Hand off to the ring. Shared transport writes directly; post transport
		// transfers the ArrayBuffers.
		ring.insert(timestamp, channelData);

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

	// Flush the audio buffer and re-stall, re-anchoring playback to the next frame.
	// Use in buffered mode at an utterance boundary (see Sync.reset).
	reset(): void {
		this.#ring?.reset();
	}

	// React to the container consumer's discontinuity counter. When it changes the publisher
	// has rewound the timeline, so flush the queued PCM and re-anchor the shared clock before
	// the first frame of the new utterance is decoded. This makes the wire signal trigger the
	// same flush as a manual `reset()`, with no app involvement.
	#onDiscontinuity(count: number): void {
		if (count === this.#discontinuity) return;
		this.#discontinuity = count;
		this.#ring?.reset();
		this.source.sync.reset();
	}

	close() {
		this.#signals.close();
	}
}

async function supported(config: Catalog.AudioConfig): Promise<boolean> {
	// Opus in CMAF uses raw packets; dOps is not a valid OGG Identification Header.
	let description: Uint8Array | undefined;
	if (config.codec !== "opus") {
		if (config.description) {
			description = Util.Hex.toBytes(config.description);
		} else if (config.container.kind === "cmaf") {
			try {
				description = Container.Cmaf.decodeInitSegment(base64ToBytes(config.container.init)).description;
			} catch (err) {
				// A malformed init segment means we can't extract the codec
				// description, so we can't probe support reliably. Reject the
				// track rather than letting isConfigSupported pass on a
				// description-less config and then having decode() fail later.
				console.warn(`audio: malformed CMAF init segment for codec ${config.codec}`, err);
				return false;
			}
		}
	}
	const res = await AudioDecoder.isConfigSupported({
		...config,
		description,
	});
	return res.supported ?? false;
}

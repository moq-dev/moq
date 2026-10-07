import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Util from "@moq/hang/util";
import type * as Moq from "@moq/net";
import { Time } from "@moq/net";
import {
	type Computed,
	Effect,
	type Getter,
	getter,
	type Inputs,
	type Readonlys,
	readonlys,
	Signal,
} from "@moq/signals";
import { hostedAssets } from "../assets";
import { base64ToBytes } from "../base64";
import { nextMedia, subscribeMedia } from "../media";

import type { Sync } from "../sync";
import { type AudioBuffer, createAudioBuffer } from "./buffer";
import { type DecoderConfig, frameDuration, type PlaybackIdentity, packetDuration, playbackIdentity } from "./config";
import { Handover } from "./handover";
import { AUTO_MAX_AGE, ringSamples, target } from "./latency";
// A blob: URL, or a hosted file when assets() is set; see vite-plugin-worklet.
import RenderWorklet from "./render-worklet.ts?worklet";
import type { Source } from "./source";
import { type DecodedSpan, Terminal } from "./terminal";
import { Warmup } from "./warmup";

const LEGACY_WARMUP_CALLBACKS = 3;

export type DecoderInput = {
	// Whether to download the audio track. Defaults to true.
	enabled: Getter<boolean>;
};

/** Constructor properties for {@link Decoder}. */
export type DecoderProps = Inputs<DecoderInput> & {
	/** Rendition selector supplying encoded audio. */
	source: Source;
	/** Shared playback clock. */
	sync: Sync;
};

type DecoderOutput = {
	context: Signal<AudioContext | undefined>;

	// The root of the audio graph, which can be used for custom visualizations.
	// Downcast to AudioNode so it matches Publish.Audio
	root: Signal<AudioNode | undefined>;

	sampleRate: Signal<number | undefined>;
	stats: Signal<Stats | undefined>;

	// Current playback timestamp from worklet
	timestamp: Signal<Time.Milli | undefined>;

	// Whether the audio buffer is stalled (waiting to fill)
	stalled: Signal<boolean>;

	// How many times the ring ran dry mid-playback, so the UI can show that the target is too low.
	underruns: Signal<number>;

	// Combined buffered ranges (network jitter + decode buffer)
	buffered: Signal<Container.BufferedRanges>;
};

// What the audio graph is built for. `catalog` is the advertised rate and `sampleRate` the rate the
// decoder actually outputs, which can differ (Opus decodes to 48kHz on Chrome/Firefox but to the
// configured rate on Safari). Until a frame arrives the graph is pre-built at the advertised rate.
type Shape = {
	catalog: number;
	sampleRate: number;
	channels: number;
};

/** Cumulative audio statistics since the decoder started. */
export interface Stats {
	/** Number of encoded bytes received. */
	bytesReceived: number;
}

/**
 * Downloads audio from a track and emits it to an AudioContext.
 *
 * The user is responsible for hooking up audio to speakers, an analyzer, etc.
 */
export class Decoder {
	readonly in: Readonlys<DecoderInput>;
	readonly source: Source;
	readonly sync: Sync;

	readonly #out: DecoderOutput = {
		context: new Signal<AudioContext | undefined>(undefined),
		root: new Signal<AudioNode | undefined>(undefined),
		sampleRate: new Signal<number | undefined>(undefined),
		stats: new Signal<Stats | undefined>(undefined),
		timestamp: new Signal<Time.Milli | undefined>(undefined),
		stalled: new Signal<boolean>(true),
		underruns: new Signal<number>(0),
		buffered: new Signal<Container.BufferedRanges>([]),
	};
	readonly out = readonlys(this.#out);

	// Decode buffer: audio sent to worklet but not yet played
	#decodeBuffered = new Signal<Container.BufferedRanges>([]);

	// Audio ring bridging main thread and worklet (shared memory or postMessage transport).
	#ring = new Signal<AudioBuffer | undefined>(undefined);

	// The context, worklet, and ring are keyed on this alone. It outlives a rendition's absence, so
	// the queued tail plays out and a return with the same shape reuses the graph.
	#shape = new Signal<Shape | undefined>(undefined);

	// The rendition is absent and the ring has drained its tail, so the graph has nothing to play.
	readonly #idle: Computed<boolean>;

	// Ordered discontinuity and endpoint state from the container consumer.
	#terminal = new Terminal();

	// The container consumer's arrival estimate, unset while nothing is subscribed.
	#measured = new Signal<Time.Milli | undefined>(undefined);

	// The subscription's max age: the shared budget, raised to the estimator's ceiling in "auto".
	#subscribeMaxAge = new Signal<Time.Milli>(Time.Milli.zero);

	// The codec's frame duration: the catalog constant, refined by each frame's own duration.
	#frame = new Signal<Time.Milli | undefined>(undefined);

	// Which subscription the ring's buffered samples came from. See #runDecoder.
	#handover = new Handover();

	// The broadcast instance the ring's timeline belongs to. See #runDecoder.
	#instance?: Moq.Broadcast.Consumer["closed"];

	#signals = new Effect();

	// The catalog fields that require a replacement subscription or decoder.
	readonly #identity: Computed<PlaybackIdentity | undefined>;

	constructor(props: DecoderProps) {
		this.in = {
			enabled: getter(props?.enabled ?? true),
		};

		this.source = props.source;
		this.sync = props.sync;
		// The "auto" playout target this track needs, per doc/concept/audio-jitter.md.
		const playout = this.#signals.computed((effect) => {
			const measured = effect.get(this.#measured);
			if (measured === undefined) return undefined;
			const delay = effect.get(this.source.out.config)?.delay;
			return target({
				measured,
				advertised: effect.get(this.source.out.jitter),
				frame: effect.get(this.#frame),
				delay: delay !== undefined ? Time.Milli(delay) : undefined,
			});
		});
		this.#signals.cleanup(this.sync.register(playout));
		this.#identity = this.#signals.computed((effect) => {
			const config = effect.get(this.source.out.config);
			return config ? playbackIdentity(config) : undefined;
		});
		this.#idle = this.#signals.computed(
			(effect) => effect.get(this.source.out.config) === undefined && effect.get(this.#out.stalled),
		);

		this.#signals.run(this.#runSubscribeMaxAge.bind(this));
		this.#signals.run(this.#runShape.bind(this));
		this.#signals.run(this.#runWorklet.bind(this));
		this.#signals.run(this.#runEnabled.bind(this));
		this.#signals.run(this.#runLatency.bind(this));
		this.#signals.run(this.#runDecoder.bind(this));
	}

	// A group the relay expires is never observed, so a subscription cut to the target would cap the
	// estimate at the target it already holds. The container consumer keeps the shared budget as its
	// local skip, since it observes each frame before applying it. See doc/concept/audio-jitter.md.
	#runSubscribeMaxAge(effect: Effect): void {
		const maxAge = effect.get(this.sync.out.maxAge);
		const auto = effect.get(this.sync.in.delay) === "auto";
		this.#subscribeMaxAge.set(auto ? Time.Milli.max(maxAge, AUTO_MAX_AGE) : maxAge);
	}

	#runShape(effect: Effect): void {
		// An absent rendition keeps the last shape, so the graph outlives it.
		const config = effect.get(this.source.out.config);
		if (!config) return;

		// A rendition matching either the advertised or the decoded rate keeps the graph, and with it the
		// rate #emit learned from the decoder.
		const shape = this.#shape.peek();
		const rate = shape?.catalog === config.sampleRate || shape?.sampleRate === config.sampleRate;
		if (rate && shape?.channels === config.numberOfChannels) return;

		this.#shape.set({
			catalog: config.sampleRate,
			sampleRate: config.sampleRate,
			channels: config.numberOfChannels,
		});
	}

	#runWorklet(effect: Effect): void {
		// It takes a second or so to initialize the AudioContext/AudioWorklet, so do it even if disabled.
		// This is less efficient for video-only playback but makes muting/unmuting instant.
		const shape = effect.get(this.#shape);
		if (!shape) return;
		// Rendition absence keeps the graph warm, but a disabled broadcast releases its resources.
		const broadcast = effect.get(this.source.in.broadcast);
		if (!broadcast || !effect.get(broadcast.in.enabled)) return;

		const { sampleRate, channels: channelCount } = shape;

		// Expose the rate the graph actually runs at.
		effect.set(this.#out.sampleRate, sampleRate);

		const context = new AudioContext({
			latencyHint: "interactive", // We don't use real-time because of the buffer.
			sampleRate,
		});
		effect.set(this.#out.context, context);

		effect.cleanup(() => context.close());

		effect.spawn(async () => {
			// Register the AudioWorklet processor, racing the load against teardown. If teardown wins,
			// `loaded` is undefined and we bail before constructing the node: the module registration was
			// abandoned, so building against its name would throw. Gate on the race result, not
			// `context.state`, because `AudioContext.close()` only flips `.state` to "closed" synchronously
			// on Chrome (Firefox/Safari report "suspended").
			const loaded = await effect.race(
				RenderWorklet(hostedAssets()).then(async (url) => {
					await context.audioWorklet.addModule(url);
					return true;
				}),
			);
			if (!loaded) return;

			// Create the worklet node. outputChannelCount must be set explicitly
			// so the process() callback receives a matching channel layout.
			// Firefox defaults differently than Chrome otherwise.
			const worklet = new AudioWorkletNode(context, "render", {
				channelCount,
				channelCountMode: "explicit",
				outputChannelCount: [channelCount],
			});
			effect.cleanup(() => worklet.disconnect());

			// Initial ring depth in samples.
			const delay = this.sync.out.delay.peek();
			const latencySamples = ringSamples(sampleRate, delay);
			const buffered = this.sync.out.buffered.peek();

			// Let the factory pick the best transport (SharedArrayBuffer or postMessage).
			const ring = createAudioBuffer(worklet, channelCount, sampleRate, latencySamples, buffered);
			effect.cleanup(() => ring.close());
			effect.set(this.#ring, ring);

			// Mirror ring state (timestamp/stalled) onto our public signals.
			effect.run((inner) => {
				const ts = Time.Milli.fromMicro(inner.get(ring.timestamp));
				this.#out.timestamp.set(ts);
				this.#trimDecodeBuffered(ts);
			});
			effect.run((inner) => {
				this.#out.stalled.set(inner.get(ring.stalled));
			});
			effect.run((inner) => {
				this.#out.underruns.set(inner.get(ring.underruns));
			});

			effect.set(this.#out.root, worklet);
		});
	}

	#runEnabled(effect: Effect): void {
		const context = effect.get(this.#out.context);

		// An idle graph stops the audio thread while it waits, so pages with many tiles pay nothing.
		// The unlock below resumes it once the rendition returns.
		if (context && effect.get(this.#idle)) {
			context.suspend().catch(() => {});
			return;
		}

		const enabled = effect.get(this.in.enabled);
		if (!enabled) return;
		if (effect.get(this.sync.out.instant)) {
			this.reset();
			return;
		}

		if (!context) return;

		// The context is built at page load (see #runWorklet), before any user gesture, so it
		// must be started from a real interaction.
		Util.Gesture.unlock(effect, context);

		// NOTE: You should disconnect/reconnect the worklet to save power when disabled.
	}

	#runLatency(effect: Effect): void {
		const ring = effect.get(this.#ring);
		if (!ring) return;

		// A rise parks playback until the ring refills to the new floor, which is what keeps audio in
		// step with video. The measured target moves a bucket at a time, and a rise the buffered
		// slack already covers costs no silence at all.
		const delay = effect.get(this.sync.out.delay);
		ring.setLatency(ringSamples(ring.rate, delay));
	}

	#runDecoder(effect: Effect): void {
		const enabled = effect.get(this.in.enabled);
		if (!enabled) return;
		if (effect.get(this.sync.out.instant)) return;

		const broadcast = effect.get(this.source.in.broadcast);
		if (!broadcast) return;

		const track = effect.get(this.source.out.track);
		if (!track) return;

		const identity = effect.get(this.#identity);
		if (!identity) return;

		const config = identity.decoder;
		this.#frame.set(frameDuration(config));

		// Honor a per-rendition `broadcast` override: subscribe on the resolved source
		// broadcast instead of the catalog's own broadcast.
		const active = broadcast.relativeBroadcast(effect, identity.broadcast);
		if (!active) return;

		// Another broadcast (a new name, or a republish) brings its own timeline, which a ring anchored
		// on the previous one would discard as already played. A return to the same instance keeps the
		// anchor, so the relay's redelivered last group is still dropped as old. Every handle to one
		// instance shares `closed`, so it identifies the instance where the handle itself does not.
		if (this.#instance !== undefined && this.#instance !== active.closed) {
			this.#ring.peek()?.reset();
			this.#decodeBuffered.set([]);
		}
		this.#instance = active.closed;

		// The ring outlives this effect (it's keyed on the sample rate and channel count), so a
		// replacement subscription on the same broadcast (a rendition swap, a return from an absence)
		// inherits whatever its predecessor decoded. Samples are timestamp indexed, so the replacement
		// overwrites the slots it lands on, but a publisher writing ahead of real-time leaves seconds
		// of tail beyond them. Drop that once the replacement's first frame says where it starts.
		this.#handover.opened();

		const sub = subscribeMedia(effect, {
			broadcast: active,
			track,
			priority: Catalog.PRIORITY.audio,
			maxAge: this.#subscribeMaxAge,
		});
		if (!sub) return;

		if (config.container.kind === "cmaf") {
			this.#runCmafDecoder(effect, sub, config);
		} else {
			this.#runLegacyDecoder(effect, sub, config);
		}
	}

	#runLegacyDecoder(effect: Effect, sub: Moq.Track.Subscriber, config: DecoderConfig): void {
		const preSkip =
			config.codec === "opus" && config.description ? Util.Opus.preSkip(Util.Hex.toBytes(config.description)) : 0;
		this.#terminal.clear(preSkip);
		const format =
			config.container.kind === "loc" ? new Container.Loc.Format("audio") : new Container.Legacy.Format(config);
		// Create consumer with slightly less latency than the render worklet to avoid underflowing.
		// TODO include JITTER_UNDERHEAD
		const consumer = new Container.Consumer(sub, {
			format,
			maxAge: this.sync.out.maxAge,
		});
		effect.cleanup(() => consumer.close());

		// Combine network jitter buffer with decode buffer
		effect.run((inner) => {
			const network = inner.get(consumer.buffered);
			const decode = inner.get(this.#decodeBuffered);
			this.#out.buffered.update(() => Container.mergeBufferedRanges(network, decode));
		});

		// Feed the arrival estimate into the playout target. Cleared on teardown so a departed track
		// stops holding the buffer open.
		effect.run((inner) => this.#measured.set(inner.get(consumer.spread)));
		effect.cleanup(() => this.#measured.set(undefined));

		effect.spawn(async () => {
			const loaded = await Util.Libav.polyfill();
			if (!loaded) return; // cancelled

			const warmup = new Warmup(LEGACY_WARMUP_CALLBACKS);

			const decoder = new AudioDecoder({
				output: (data) => {
					const decoded = this.#terminal.span(data);
					if (warmup.drop()) {
						// Drop initial callbacks to prime the decoder.
						data.close();
						return;
					}
					this.#emit(data, decoded);
				},
				error: (error) => console.error("audio decoder error", error),
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
			const decoderConfig: AudioDecoderConfig = {
				codec: config.codec,
				sampleRate: config.sampleRate,
				numberOfChannels: config.numberOfChannels,
				description,
			};
			decoder.configure(decoderConfig);

			for (;;) {
				const next = await nextMedia(consumer);
				if (!next) break;
				if (this.#onNext(next)) {
					decoder.reset();
					decoder.configure(decoderConfig);
				}
				if (next.end !== undefined) {
					continue;
				}

				const { frame } = next;
				if (!frame) continue;

				// Mark that we received this frame right now.
				const timestamp = Time.Milli.fromMicro(frame.timestamp as Time.Micro);
				this.sync.received(timestamp, "audio");

				const duration = packetDuration(config.codec, frame);
				if (duration !== undefined) this.#frame.set(duration);

				this.#out.stats.update((stats) => ({
					bytesReceived: (stats?.bytesReceived ?? 0) + frame.payload.byteLength,
				}));

				const ring = await this.#ready(effect);
				if (!ring) break;

				// Backpressure: in buffered mode this holds the encoded frame until the playhead nears
				// it, keeping the lookahead above the floor as Opus instead of decoded PCM. No-op live.
				await ring.wait(frame.timestamp as Time.Micro);

				const chunk = new EncodedAudioChunk({
					type: frame.keyframe ? "key" : "delta",
					data: frame.payload,
					timestamp: frame.timestamp,
				});

				// A fatal decode error closes the decoder, so decoding again throws InvalidStateError out
				// of this loop. Stop instead: the error callback already reported the real failure.
				if (decoder.state === "closed") break;
				decoder.decode(chunk);
			}
		});
	}

	#runCmafDecoder(effect: Effect, sub: Moq.Track.Subscriber, config: DecoderConfig): void {
		if (config.container.kind !== "cmaf") return; // just to help typescript

		const initSegment = base64ToBytes(config.container.init);
		const init = Container.Cmaf.decodeInitSegment(initSegment);
		const opusDescription = config.description ? Util.Hex.toBytes(config.description) : init.description;
		const preSkip = config.codec === "opus" && opusDescription ? Util.Opus.preSkip(opusDescription) : 0;
		this.#terminal.clear(preSkip);
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
			maxAge: this.sync.out.maxAge,
		});
		effect.cleanup(() => consumer.close());

		// Combine network jitter buffer with decode buffer
		effect.run((inner) => {
			const network = inner.get(consumer.buffered);
			const decode = inner.get(this.#decodeBuffered);
			this.#out.buffered.update(() => Container.mergeBufferedRanges(network, decode));
		});

		// Feed the arrival estimate into the playout target. Cleared on teardown so a departed track
		// stops holding the buffer open.
		effect.run((inner) => this.#measured.set(inner.get(consumer.spread)));
		effect.cleanup(() => this.#measured.set(undefined));

		effect.spawn(async () => {
			const loaded = await Util.Libav.polyfill();
			if (!loaded) return; // cancelled

			const decoder = new AudioDecoder({
				output: (data) => this.#emit(data),
				error: (error) => console.error("audio decoder error", error),
			});
			effect.cleanup(() => {
				if (decoder.state !== "closed") decoder.close();
			});

			// Configure decoder with description from catalog
			const decoderConfig: AudioDecoderConfig = {
				codec: config.codec,
				sampleRate: config.sampleRate,
				numberOfChannels: config.numberOfChannels,
				description,
			};
			decoder.configure(decoderConfig);

			for (;;) {
				const next = await nextMedia(consumer);
				if (!next) break;

				// Reset and re-anchor before decoding the first frame of a new codec epoch.
				if (this.#onNext(next)) {
					decoder.reset();
					decoder.configure(decoderConfig);
				}

				const { frame } = next;
				if (!frame) continue;

				const timestamp = Time.Milli.fromMicro(frame.timestamp);
				this.sync.received(timestamp, "audio");

				const duration = packetDuration(config.codec, frame);
				if (duration !== undefined) this.#frame.set(duration);

				this.#out.stats.update((stats) => ({
					bytesReceived: (stats?.bytesReceived ?? 0) + frame.payload.byteLength,
				}));

				const ring = await this.#ready(effect);
				if (!ring) break;

				// Backpressure: in buffered mode this holds the encoded frame until the playhead nears
				// it, keeping the lookahead above the floor as Opus instead of decoded PCM. No-op live.
				await ring.wait(frame.timestamp);

				if (decoder.state === "closed") break;
				decoder.decode(
					new EncodedAudioChunk({
						type: frame.keyframe ? "key" : "delta",
						data: frame.payload,
						timestamp: frame.timestamp,
					}),
				);
			}
		});
	}

	// The ring, once the worklet has loaded. Decoding waits for it, so the frames that arrive first
	// queue in the consumer instead of being decoded into nowhere. Undefined once `effect` is torn down.
	async #ready(effect: Effect): Promise<AudioBuffer | undefined> {
		for (;;) {
			const ring = this.#ring.peek();
			if (ring) return ring;
			await effect.race(Signal.race(this.#ring));
			if (effect.abort.aborted) return undefined;
		}
	}

	#emit(sample: AudioData, decoded: DecodedSpan = this.#terminal.span(sample)) {
		const { timestamp, frameOffset, frames } = decoded;
		const timestampMilli = Time.Milli.fromMicro(timestamp);
		if (frames === 0) {
			sample.close();
			return;
		}

		const ring = this.#ring.peek();
		if (!ring) {
			// The graph is being rebuilt or closed.
			sample.close();
			return;
		}

		// sample.sampleRate is the source of truth, and it can differ from the rate we pre-built the
		// graph against (see Shape). If they disagree, rebuild the graph at the real rate and drop this
		// frame; the ring being torn down can't accept it, and the next frame lands in the new ring.
		if (sample.sampleRate !== ring.rate) {
			this.#shape.update((shape) => shape && { ...shape, sampleRate: sample.sampleRate });
			sample.close();
			return;
		}

		// Calculate end time from sample duration
		const durationMicro = ((frames / sample.sampleRate) * 1_000_000) as Time.Micro;
		const durationMilli = Time.Milli.fromMicro(durationMicro);
		const end = Time.Milli.add(timestampMilli, durationMilli);

		// A new subscription has taken over the timeline: drop the previous one's write-ahead tail
		// rather than letting it play out after this frame. See #runDecoder.
		if (this.#handover.takeover()) {
			ring.truncate(timestamp);
			this.#truncateDecodeBuffered(timestampMilli);
		}

		// Add to decode buffer
		this.#addDecodeBuffered(timestampMilli, end);

		// Firefox's Opus decoder sometimes outputs more channels than requested
		// (e.g. 6 for stereo). Clamp to the ring's channel count.
		const channels = Math.min(sample.numberOfChannels, ring.channels);
		const channelData: Float32Array[] = [];
		for (let channel = 0; channel < channels; channel++) {
			const data = new Float32Array(frames);
			sample.copyTo(data, { format: "f32-planar", planeIndex: channel, frameOffset, frameCount: frames });
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

	// Drop reported decode ranges at or after `timestamp`, mirroring a ring truncation.
	#truncateDecodeBuffered(timestamp: Time.Milli): void {
		this.#decodeBuffered.mutate((current) => {
			while (current.length > 0 && current[current.length - 1].start >= timestamp) current.pop();
			const last = current[current.length - 1];
			if (last && last.end > timestamp) last.end = timestamp;
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
		this.#ring.peek()?.reset();
	}

	// Apply ordered container metadata before handling the result. An endpoint that also
	// starts a new epoch must survive the reset so its following drain is trimmed.
	#onNext(next: {
		discontinuity: number;
		group: number;
		end?: Time.Micro;
		frame?: { timestamp: Time.Micro };
	}): boolean {
		if (!this.#terminal.update(next)) return false;
		this.#ring.peek()?.reset();
		this.sync.reset();
		return true;
	}

	close() {
		this.#signals.close();
	}

	// Whether the WebCodecs audio decoder can play this config.
	static supported = supported;
}

async function supported(config: Catalog.AudioConfig): Promise<boolean> {
	if (!Catalog.containerSupported(config.container)) {
		// `kind` is the literal "unknown" tag; the container the publisher actually named is in `raw`.
		const kind = config.container.kind === "unknown" ? config.container.raw.kind : config.container.kind;
		console.warn(`audio: ignoring rendition with unknown container: ${kind}`);
		return false;
	}

	// Opus only runs at its native rates, so a catalog advertising anything else is wrong and Safari
	// refuses to decode it. Warn rather than reject: Chrome and Firefox ignore the configured rate and
	// play these streams fine, so rejecting would silence them for a publisher they handle today.
	if (config.codec === "opus" && !Util.Opus.supportsRate(config.sampleRate)) {
		console.warn(`audio: opus advertised at ${config.sampleRate}Hz, which some browsers cannot decode`);
	}

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

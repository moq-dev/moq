import { Micro, Milli, type Second } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Util from "@moq/hang/util";
import type * as Moq from "@moq/net";
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
import { Producer as ArrivalProducer, type Arrivals, gap } from "../arrivals";
import { hostedAssets } from "../assets";
import { base64ToBytes } from "../base64";
import { type Failure, failureMoved, nextMedia, subscribeMedia } from "../media";
import type { Sync } from "../sync";
import { AudioBuffer } from "./buffer";
import { type DecoderConfig, frameDuration, type PlaybackIdentity, packetDuration, playbackIdentity } from "./config";
import { Handover } from "./handover";
import { AUTO_MAX_DELAY, ringHold, target } from "./latency";
import { Order } from "./order";
// A blob: URL, or a hosted file when assets() is set; see vite-plugin-worklet.
import RenderWorklet from "./render-worklet.ts?worklet";
import type { Source } from "./source";
import { Terminal } from "./terminal";

type FrameRead = Extract<Container.Read, { kind: "frame" }>;

type Queued = {
	newest: Micro | undefined;
	buffered: Container.BufferedRanges;
};

/** Signals the decoder reads. */
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

/** Signals the decoder derives. */
export type DecoderOutput = {
	/** Fatal decode failure, cleared when another source is selected. */
	error: Signal<Error | undefined>;
	context: Signal<AudioContext | undefined>;

	// The root of the audio graph, which can be used for custom visualizations.
	// Downcast to AudioNode so it matches Publish.Audio
	root: Signal<AudioNode | undefined>;

	sampleRate: Signal<number | undefined>;
	stats: Signal<Stats | undefined>;

	// Current playback timestamp from worklet
	timestamp: Signal<Milli | undefined>;

	// Whether the audio buffer is stalled (waiting to fill)
	stalled: Signal<boolean>;

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
	/** How many times playback ran dry mid-stream; a rising count means the delay is too low. */
	underruns: number;
}

/**
 * Downloads audio from a track and emits it to an AudioContext.
 *
 * The user is responsible for hooking up audio to speakers, an analyzer, etc.
 */
export class Decoder {
	/** The wired inputs, read-only. */
	readonly in: Readonlys<DecoderInput>;
	/** The source picking the rendition to decode. */
	readonly source: Source;
	/** The clock that schedules playback. */
	readonly sync: Sync;

	readonly #out: DecoderOutput = {
		error: new Signal<Error | undefined>(undefined),
		context: new Signal<AudioContext | undefined>(undefined),
		root: new Signal<AudioNode | undefined>(undefined),
		sampleRate: new Signal<number | undefined>(undefined),
		stats: new Signal<Stats | undefined>(undefined),
		timestamp: new Signal<Milli | undefined>(undefined),
		stalled: new Signal<boolean>(true),
		buffered: new Signal<Container.BufferedRanges>([]),
	};
	/** The derived outputs, read-only. */
	readonly out = readonlys(this.#out);

	// Decode buffer: audio sent to worklet but not yet played
	#decodeBuffered = new Signal<Container.BufferedRanges>([]);
	#queued = new Signal<Container.BufferedRanges | undefined>(undefined);

	// Audio ring bridging the main thread and the worklet.
	#ring = new Signal<AudioBuffer | undefined>(undefined);

	// The context, worklet, and ring are keyed on this alone. It outlives a rendition's absence, so
	// the queued tail plays out and a return with the same shape reuses the graph.
	#shape = new Signal<Shape | undefined>(undefined);

	// The rendition is absent and the ring has drained its tail, so the graph has nothing to play.
	readonly #idle: Computed<boolean>;

	// Ordered epoch and endpoint state from the container consumer.
	#terminal = new Terminal();

	// The container consumer's arrival estimate, unset while nothing is subscribed.
	#measured = new Signal<Milli | undefined>(undefined);

	// The newest container consumer, the path it reads, and its codec, whose estimate a replacement
	// continues.
	#consumer?: { path: Moq.Path.Valid; codec: string; consumer: Container.Consumer };

	// The subscription's max delay: the shared budget, raised to the estimator's ceiling in "auto".
	#subscribeMaxDelay = new Signal<Milli>(Milli.zero);
	#maxDelay = new Signal<Milli>(Milli.zero);

	// The codec's frame duration: the catalog constant, refined by each frame's own duration.
	#frame = new Signal<Milli | undefined>(undefined);

	// Which subscription the ring's buffered samples came from. See #runDecoder.
	#handover = new Handover();

	// The broadcast instance the ring's timeline belongs to. See #runDecoder.
	#instance?: Moq.Broadcast.Consumer["closed"];

	// Fans received frames out to open `arrivals()` handles; retains nothing without one.
	#arrivals = new ArrivalProducer();

	#signals = new Effect();
	#failed?: Failure;

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
				delay: delay !== undefined ? Milli(delay) : undefined,
			});
		});
		const startup = this.#signals.computed((effect) => {
			if (!effect.get(this.in.enabled)) return undefined;
			if (effect.get(this.source.pending)) return null;
			// A held disabled rendition has no frames coming, so startup does not wait on it.
			const config = effect.get(this.source.out.config);
			if (!config || config.enabled === false) return undefined;
			const span = effect.get(this.#decodeBuffered)[0];
			if (!span) return null;
			// Deep targets stay encoded, so count the audio queued past the decoded span. Sum it across
			// holes: a lost packet must not hold startup, and the missing audio is not counted as buffered.
			let queued = 0;
			for (const range of effect.get(this.#queued) ?? []) {
				queued += Math.max(0, range.end - Math.max(range.start, span.end));
			}
			const hold = Milli.fromMicro(ringHold(effect.get(this.sync.out.delay)));
			return { start: span.start, end: Milli(span.end + Math.min(queued, hold)) };
		});
		this.#signals.cleanup(this.sync.register(playout, startup));
		this.#identity = this.#signals.computed((effect) => {
			const config = effect.get(this.source.out.config);
			return config ? playbackIdentity(config) : undefined;
		});
		// A held disabled rendition idles too, so a muted tile suspends its AudioContext.
		this.#idle = this.#signals.computed((effect) => {
			const config = effect.get(this.source.out.config);
			return (!config || config.enabled === false) && effect.get(this.#out.stalled);
		});

		this.#signals.run(this.#runSubscribeMaxDelay.bind(this));
		this.#signals.run(this.#runShape.bind(this));
		this.#signals.run(this.#runWorklet.bind(this));
		this.#signals.run(this.#runEnabled.bind(this));
		this.#signals.run(this.#runLatency.bind(this));
		this.#signals.run(this.#runDecoder.bind(this));
	}

	// A group the relay expires is never observed, so a subscription cut to the target would cap the
	// estimate at the target it already holds. The container consumer keeps the shared budget as its
	// local skip, since it observes each frame before applying it. See doc/concept/audio-jitter.md.
	#runSubscribeMaxDelay(effect: Effect): void {
		const maxDelay = effect.get(this.sync.out.maxDelay);
		const auto = effect.get(this.sync.in.delay) === "auto";
		const warmup = Milli(effect.get(this.source.out.config)?.warmup ?? 0);
		this.#maxDelay.set(Milli(maxDelay + warmup));
		this.#subscribeMaxDelay.set(Milli((auto ? Math.max(maxDelay, AUTO_MAX_DELAY) : maxDelay) + warmup));
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
		const buffered = effect.get(this.sync.out.buffered);

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

			const ring = new AudioBuffer(worklet, {
				channels: channelCount,
				rate: sampleRate,
				delay: this.sync.out.delay.peek(),
				buffered,
				playing: this.sync.playing,
			});
			effect.cleanup(() => ring.close());
			effect.set(this.#ring, ring);

			// Mirror ring state (timestamp/stalled) onto our public signals.
			effect.run((inner) => {
				const ts = Milli.fromMicro(inner.get(ring.timestamp));
				this.#out.timestamp.set(ts);
				this.#trimDecodeBuffered(ts);
			});
			effect.run((inner) => {
				this.#out.stalled.set(inner.get(ring.stalled));
			});
			// Each ring counts from zero, so add only what this one counted to the cumulative stats.
			let counted = 0;
			effect.run((inner) => {
				const underruns = inner.get(ring.underruns);
				const added = underruns - counted;
				counted = underruns;
				if (added <= 0) return;
				this.#out.stats.update((stats) => ({
					bytesReceived: stats?.bytesReceived ?? 0,
					underruns: (stats?.underruns ?? 0) + added,
				}));
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

		// A rise parks playback until the queued media covers the new target, which is what keeps
		// audio in step with video. The measured target moves a bucket at a time, and a rise the
		// buffered slack already covers costs no silence at all.
		ring.setLatency(effect.get(this.sync.out.delay));
	}

	#runDecoder(effect: Effect): void {
		// The reorder policy belongs to the playback mode, so a mode change starts a new reader.
		effect.get(this.sync.out.buffered);
		const running = effect.get(this.in.enabled) && !effect.get(this.sync.out.instant);
		const broadcast = effect.get(this.source.in.broadcast);
		const track = effect.get(this.source.out.track);
		const identity = effect.get(this.#identity);

		// Honor a per-rendition `broadcast` override: subscribe on the resolved source
		// broadcast instead of the catalog's own broadcast. A stopped decoder resolves it only to
		// tell whether a failure's source is still selected.
		const active =
			broadcast && track && identity && (running || this.#failed)
				? broadcast.relativeBroadcast(effect, identity.broadcast, identity.epoch)
				: undefined;
		const key = identity && JSON.stringify(identity);

		// A failure belongs to its source: selecting another clears it, even while muted or paused.
		const failed = this.#failed;
		const selected = broadcast && failed ? effect.get(broadcast.out.active)?.closed : undefined;
		if (failed && failureMoved(failed, { broadcast: selected, media: active?.closed, track, identity: key })) {
			this.#failed = undefined;
			this.#out.error.set(undefined);
		}

		if (!running || !broadcast || !track || !identity) {
			this.#retire();
			return;
		}

		const config = identity.decoder;

		if (!active || !key) {
			this.#retire();
			return;
		}

		// This source already failed; wait for another.
		if (this.#failed) {
			this.#retire();
			return;
		}
		// Closing this effect releases both its container reader and track subscription.
		const failure = new Signal<Error | undefined>(undefined);
		let attempt: Effect | undefined;
		effect.run((inner) => {
			const error = inner.get(failure);
			if (!error) return;
			this.#failed = {
				broadcast: broadcast.out.active.peek()?.closed,
				media: active.closed,
				track,
				identity: key,
			};
			this.#out.error.set(error);
			attempt?.close();
			this.#retire();
		});

		// Another broadcast (a new name, or a republish) brings its own timeline, which a ring and a
		// clock anchored on the previous one would discard as already played. A return to the same
		// instance keeps the anchor, so the relay's redelivered last group is still dropped as old.
		// Every handle to one instance shares `closed`, so it identifies the instance where the
		// handle itself does not.
		if (this.#instance !== undefined && this.#instance !== active.closed) {
			this.sync.reset();
			this.reset();
		}
		this.#instance = active.closed;

		// The ring outlives this effect (it's keyed on the sample rate, channel count, and mode), so a
		// replacement subscription on the same broadcast (a rendition swap, a return from an absence)
		// inherits whatever its predecessor decoded. Samples are timestamp indexed, so the replacement
		// overwrites the slots it lands on, but a publisher writing ahead of real-time leaves seconds
		// of tail beyond them. Drop that once the replacement's first frame says where it starts.
		this.#handover.opened();

		effect.run((run) => {
			attempt = run;
			const sub = subscribeMedia(run, {
				broadcast: active,
				track: Catalog.track(track, identity),
				priority: Catalog.PRIORITY.audio,
				maxDelay: this.#subscribeMaxDelay,
			});
			if (!sub) {
				this.#retire();
				return;
			}

			// A config the probe accepted can still fail to parse (a malformed init segment or Opus
			// header). Report it like a decode failure, which also retires the outgoing estimate.
			const props = { config, failure, path: active.path };
			spawnFatal(run, failure, async () => {
				if (config.container.kind === "cmaf") await this.#runCmafDecoder(run, sub, props);
				else this.#runLegacyDecoder(run, sub, props);
			});
		});
	}

	#runLegacyDecoder(
		effect: Effect,
		sub: Moq.Track.Subscriber,
		{ config, failure, path }: { config: DecoderConfig; failure: Signal<Error | undefined>; path: Moq.Path.Valid },
	): void {
		const preSkip =
			config.codec === "opus" && config.description ? Util.Opus.preSkip(Util.Hex.toBytes(config.description)) : 0;
		this.#terminal.clear(preSkip, Micro.fromMilli(Milli(config.warmup ?? 0)));
		const format =
			config.container.kind === "loc" ? new Container.Loc.Format("audio") : new Container.Legacy.Format(config);
		// Create consumer with slightly less latency than the render worklet to avoid underflowing.
		// TODO include JITTER_UNDERHEAD
		const { consumer, read } = this.#consume(effect, sub, format, { config, path });

		spawnFatal(effect, failure, async () => {
			const loaded = await Util.Libav.polyfill();
			if (!loaded) return; // cancelled

			const decoder = new AudioDecoder({
				output: (data) => this.#emit(data),
				error: (error) => failure.set(error),
			});
			effect.cleanup(() => {
				if (decoder.state !== "closed") decoder.close();
				// Closing drops what is still decoding, which a refill would otherwise wait on.
				this.#ring.peek()?.dropped();
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

			const order = this.#order();
			const play = (reads: FrameRead[]) => {
				read.set({ newest: order.newest(reads) as Micro | undefined, buffered: queuedRanges(reads, config) });
				return this.#play(effect, decoder, decoderConfig, order, reads);
			};
			for (;;) {
				const next = await nextMedia(consumer);
				if (next?.kind !== "frame") {
					// The track ending, or a gap or end, cuts a held head short.
					if (!(await play(order.flush()))) return;
					if (!next) break;
					if (this.#onRead(next)) {
						this.#reprime(decoder, decoderConfig, order);
					}
					if (next.kind === "gap") this.#arrivals.write(gap(next));
					continue;
				}
				const { frame } = next;

				// Classify ingress before decoder backpressure changes the delivery clock.
				const timestamp = Milli.fromMicro(frame.timestamp as Micro);
				const late = this.sync.received(timestamp, frame.arrival);
				this.#arrivals.write({ kind: "frame", timestamp, arrival: frame.arrival, late });

				const duration = packetDuration(config.codec, frame);
				if (duration !== undefined) this.#frame.set(duration);

				this.#out.stats.update((stats) => ({
					bytesReceived: (stats?.bytesReceived ?? 0) + frame.payload.byteLength,
					underruns: stats?.underruns ?? 0,
				}));

				if (!(await play(order.push(next)))) return;
			}
		});
	}

	async #runCmafDecoder(
		effect: Effect,
		sub: Moq.Track.Subscriber,
		{ config, failure, path }: { config: DecoderConfig; failure: Signal<Error | undefined>; path: Moq.Path.Valid },
	): Promise<void> {
		if (config.container.kind !== "cmaf") return; // just to help typescript

		const abort = effect.abort;
		const Cmaf = await import("@moq/hang/cmaf");
		if (abort.aborted) return;

		const initSegment = base64ToBytes(config.container.init);
		const init = Cmaf.decodeInitSegment(initSegment);
		const opusDescription = config.description ? Util.Hex.toBytes(config.description) : init.description;
		const preSkip = config.codec === "opus" && opusDescription ? Util.Opus.preSkip(opusDescription) : 0;
		this.#terminal.clear(preSkip, Micro.fromMilli(Milli(config.warmup ?? 0)));
		// Opus in CMAF uses raw packets (not OGG-wrapped), so description must be omitted.
		// The dOps box from the init segment is not a valid OGG Identification Header.
		const description =
			config.codec === "opus"
				? undefined
				: config.description
					? Util.Hex.toBytes(config.description)
					: init.description;

		const { consumer, read } = this.#consume(effect, sub, new Cmaf.Format(init), { config, path });

		spawnFatal(effect, failure, async () => {
			const loaded = await Util.Libav.polyfill();
			if (!loaded) return; // cancelled

			const decoder = new AudioDecoder({
				output: (data) => this.#emit(data),
				error: (error) => failure.set(error),
			});
			effect.cleanup(() => {
				if (decoder.state !== "closed") decoder.close();
				// Closing drops what is still decoding, which a refill would otherwise wait on.
				this.#ring.peek()?.dropped();
			});

			// Configure decoder with description from catalog
			const decoderConfig: AudioDecoderConfig = {
				codec: config.codec,
				sampleRate: config.sampleRate,
				numberOfChannels: config.numberOfChannels,
				description,
			};
			decoder.configure(decoderConfig);

			const order = this.#order();
			const play = (reads: FrameRead[]) => {
				read.set({ newest: order.newest(reads) as Micro | undefined, buffered: queuedRanges(reads, config) });
				return this.#play(effect, decoder, decoderConfig, order, reads);
			};
			for (;;) {
				const next = await nextMedia(consumer);
				if (next?.kind !== "frame") {
					// The track ending, or a gap or end, cuts a held head short.
					if (!(await play(order.flush()))) return;
					if (!next) break;
					if (this.#onRead(next)) {
						this.#reprime(decoder, decoderConfig, order);
					}
					if (next.kind === "gap") this.#arrivals.write(gap(next));
					continue;
				}
				const { frame } = next;

				const timestamp = Milli.fromMicro(frame.timestamp);
				const late = this.sync.received(timestamp, frame.arrival);
				this.#arrivals.write({ kind: "frame", timestamp, arrival: frame.arrival, late });

				const duration = packetDuration(config.codec, frame);
				if (duration !== undefined) this.#frame.set(duration);

				this.#out.stats.update((stats) => ({
					bytesReceived: (stats?.bytesReceived ?? 0) + frame.payload.byteLength,
					underruns: stats?.underruns ?? 0,
				}));

				if (!(await play(order.push(next)))) return;
			}
		});
	}

	// Decode released frames, resetting the decoder before the first frame of a new codec epoch and
	// only then dropping one that is out of order. The terminal sees them in decode order, so it
	// anchors on the oldest. Reports whether to keep going.
	async #play(
		effect: Effect,
		decoder: AudioDecoder,
		config: AudioDecoderConfig,
		order: Order<FrameRead>,
		reads: FrameRead[],
	): Promise<boolean> {
		for (const read of reads) {
			if (this.#onRead(read)) {
				this.#reprime(decoder, config, order);
			}
			if (!order.admit(read)) continue;
			if (!(await this.#decode(effect, decoder, read.frame))) return false;
		}
		return true;
	}

	// A reset discards queued codec output, so release its accounting without clearing retained PCM.
	#reprime(decoder: AudioDecoder, config: AudioDecoderConfig, order: Order<FrameRead>): void {
		decoder.reset();
		this.#ring.peek()?.dropped();
		decoder.configure(config);
		order.reset();
	}

	// Decode one frame once backpressure admits it, reporting whether to keep going.
	async #decode(effect: Effect, decoder: AudioDecoder, frame: Container.ReceivedFrame): Promise<boolean> {
		const ring = await this.#ready(effect);
		if (!ring) return false;

		// Backpressure: this holds the encoded frame until the playhead nears it or, for the part of
		// the target above the decoded floor, until enough media is queued behind it. A no-op for live
		// playback whose whole target fits the floor.
		await ring.wait(frame.timestamp, effect.abort);
		if (effect.abort.aborted) return false;

		// A fatal decode error closes the decoder, so decoding again throws InvalidStateError out
		// of the loop. Stop instead: the error callback already reported the real failure.
		if (decoder.state === "closed") return false;
		decoder.decode(
			new EncodedAudioChunk({
				// WebCodecs audio chunks are key even inside a MoQ group.
				type: "key",
				data: frame.payload,
				timestamp: frame.timestamp,
			}),
		);
		return true;
	}

	// A buffered subscription holds its head until it runs past the floor, which playback waits for
	// anyway, or spans the window the relay serves.
	#order(): Order<FrameRead> {
		if (!this.sync.out.buffered.peek()) return new Order(undefined);
		return new Order(() => ({
			floor: Micro.fromMilli(this.sync.out.delay.peek()),
			maxDelay: Micro.fromMilli(this.#subscribeMaxDelay.peek()),
		}));
	}

	// Reorders groups and frames up to the shared max delay. A rename is not a cold start: the path's
	// arrivals are what they were, so the estimate continues from whatever read it before. Another
	// path's arrivals have nothing to do with what this one learned.
	#consume(
		effect: Effect,
		sub: Moq.Track.Subscriber,
		format: Container.Format,
		{ config, path }: { config: DecoderConfig; path: Moq.Path.Valid },
	): { consumer: Container.Consumer; read: Signal<Queued> } {
		const outgoing = this.#consumer;
		const continues = outgoing?.path === path;
		const consumer = new Container.Consumer(sub, {
			format,
			maxDelay: this.#maxDelay,
			predecessor: continues ? outgoing.consumer : undefined,
		});
		effect.cleanup(() => consumer.close());
		this.#consumer = { path, codec: config.codec, consumer };

		// Opus states its duration only per packet, so the same codec continuing keeps the outgoing
		// one until the first packet refines it. Anything else starts from the codec's own constant.
		const frame = frameDuration(config);
		if (frame !== undefined || !continues || outgoing.codec !== config.codec) this.#frame.set(frame);

		// Combine network jitter buffer with decode buffer
		effect.run((inner) => {
			const network = inner.get(consumer.buffered);
			const decode = inner.get(this.#decodeBuffered);
			this.#out.buffered.update(() => Container.mergeBufferedRanges(network, decode));
		});

		// What has arrived but not been decoded is what covers the part of the target held encoded, so
		// the decode loop's held frame is released once enough is queued behind it. That includes the
		// frames the loop has read but not decoded yet: a buffered head waits in the reorder buffer
		// until it spans the whole target, leaving the consumer empty.
		const read = new Signal<Queued>({ newest: undefined, buffered: [] });
		effect.run((inner) => {
			const ring = inner.get(this.#ring);
			if (!ring) return;
			const pending = inner.get(read);
			const network = inner.get(consumer.buffered);
			let newest = pending.newest;
			for (const { end } of network) {
				const micro = Micro.fromMilli(end);
				if (newest === undefined || micro > newest) newest = micro;
			}
			inner.set(this.#queued, Container.mergeBufferedRanges(network, pending.buffered));
			ring.queued(newest);
		});

		// Feed the arrival estimate into the playout target. It outlives this consumer, so the target
		// holds until the replacement takes over; see #retire.
		effect.run((inner) => this.#measured.set(inner.get(consumer.spread)));
		return { consumer, read };
	}

	// No consumer replaces the outgoing one, so its estimate stops holding the buffer open.
	#retire(): void {
		this.#consumer = undefined;
		this.#measured.set(undefined);
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

	#emit(sample: AudioData) {
		// Whatever becomes of it, the output answers the frames it was decoded from, so the gate counts
		// only what still decodes. Reported after any insert, so the gate always counts the frame as
		// either decoding or in the ring.
		const decoded = (sample.timestamp +
			Micro.fromSecond((sample.numberOfFrames / sample.sampleRate) as Second)) as Micro;
		let landed: Micro | undefined;
		try {
			landed = this.#place(sample);
		} finally {
			this.#ring.peek()?.decoded(decoded, landed);
		}
	}

	// Trim the output and write it to the ring, returning where the PCM written ends.
	#place(sample: AudioData): Micro | undefined {
		const { timestamp, frameOffset, frames } = this.#terminal.span(sample);
		const timestampMilli = Milli.fromMicro(timestamp);
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
		const durationMicro = ((frames / sample.sampleRate) * 1_000_000) as Micro;
		const durationMilli = Milli.fromMicro(durationMicro);
		const end = Milli(timestampMilli + durationMilli);

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
			if (frameOffset === 0) {
				const data = new Float32Array(frames);
				sample.copyTo(data, { format: "f32-planar", planeIndex: channel, frameCount: frames });
				channelData.push(data);
				continue;
			}
			// WebKit's GStreamer port (Linux WebKitGTK and WPE) reads out of bounds converting
			// interleaved audio to planar at a nonzero frameOffset, and its hardened builds abort the
			// page. Opus pre-skip trims the first frame of every stream, so copy the whole plane and
			// trim it here.
			const data = new Float32Array(sample.numberOfFrames);
			sample.copyTo(data, { format: "f32-planar", planeIndex: channel });
			channelData.push(data.slice(frameOffset, frameOffset + frames));
		}

		// Hand off to the ring, which transfers the ArrayBuffers.
		ring.insert(timestamp, channelData);

		sample.close();
		return (timestamp + durationMicro) as Micro;
	}

	#addDecodeBuffered(start: Milli, end: Milli): void {
		if (start > end) return;

		this.#decodeBuffered.mutate((current) => {
			for (const range of current) {
				// Extend range if new sample overlaps or is adjacent (1ms tolerance for float precision)
				if (start <= range.end + 1 && end >= range.start) {
					range.start = Milli(Math.min(range.start, start));
					range.end = Milli(Math.max(range.end, end));
					return;
				}
			}

			current.push({ start, end });
			current.sort((a, b) => a.start - b.start);
		});
	}

	// Drop reported decode ranges at or after `timestamp`, mirroring a ring truncation.
	#truncateDecodeBuffered(timestamp: Milli): void {
		this.#decodeBuffered.mutate((current) => {
			while (current.length > 0 && current[current.length - 1].start >= timestamp) current.pop();
			const last = current[current.length - 1];
			if (last && last.end > timestamp) last.end = timestamp;
		});
	}

	#trimDecodeBuffered(timestamp: Milli): void {
		this.#decodeBuffered.mutate((current) => {
			while (current.length > 0) {
				if (current[0].end >= timestamp) {
					current[0].start = Milli(Math.max(current[0].start, timestamp));
					break;
				}
				current.shift();
			}
		});
	}

	/**
	 * Flush the audio buffer and re-stall, re-anchoring playback to the next frame.
	 * Use in buffered mode at an utterance boundary (see Sync.reset).
	 */
	reset(): void {
		this.#decodeBuffered.set([]);
		this.#queued.set(undefined);
		this.#ring.peek()?.reset();
	}

	// Apply an ordered container read before handling it, reporting whether a codec epoch ended:
	// a gap, or a later group after a declared end.
	#onRead(read: Container.Read): boolean {
		if (!this.#terminal.update(read)) return false;
		// A hole restarts codec prediction on the same timeline, keeping decoded PCM and parked video.
		if (read.kind === "frame") {
			this.sync.reset();
			this.reset();
		}
		return true;
	}

	/**
	 * Observe every frame and gap received from now until the handle or this decoder closes.
	 *
	 * Each open handle queues every entry until read, so read it continuously or close it.
	 */
	arrivals(): Arrivals {
		return this.#arrivals.consume();
	}

	/** Stop and release everything this holds. */
	close() {
		this.#arrivals.close();
		this.#signals.close();
	}

	/** Whether the WebCodecs audio decoder can play this config. */
	static supported = supported;
}

/** Whether the WebCodecs audio decoder can play this config. */
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
				const Cmaf = await import("@moq/hang/cmaf");
				description = Cmaf.decodeInitSegment(base64ToBytes(config.container.init)).description;
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
	// Install the WebCodecs polyfill before probing, since playback would decode through it where
	// AudioDecoder is missing (Node, Bun, older browsers). Loads nothing where WebCodecs audio is native.
	await Util.Libav.polyfill();
	const res = await AudioDecoder.isConfigSupported({
		...config,
		description,
	});
	return res.supported ?? false;
}

// Run `task` for one decode attempt. A setup, read, or decode failure that is not the attempt's
// cancellation fails its source through `failure`, which latches it and tears the attempt down.
function spawnFatal(effect: Effect, failure: Signal<Error | undefined>, task: () => Promise<void>): void {
	effect.spawn(async () => {
		try {
			await task();
		} catch (error) {
			if (effect.abort.aborted) return;
			failure.set(error instanceof Error ? error : new Error(String(error)));
		}
	});
}

// Released reads are timestamp ordered and remain owned by the decode loop while it waits. Count
// their actual duration, not the distance to the next packet, which could include missing audio.
function queuedRanges(reads: FrameRead[], config: DecoderConfig): Container.BufferedRanges {
	const ranges: Container.BufferedRanges = [];
	for (const { frame } of reads) {
		const start = Milli.fromMicro(frame.timestamp);
		const duration = packetDuration(config.codec, frame) ?? frameDuration(config) ?? Milli.zero;
		const end = Milli(start + duration);
		const last = ranges.at(-1);
		if (last && start <= last.end + 1) last.end = Milli(Math.max(last.end, end));
		else ranges.push({ start, end });
	}
	return ranges;
}

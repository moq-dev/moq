import { Duration, type Wall } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Util from "@moq/hang/util";
import type * as Moq from "@moq/net";
import {
	type Computed,
	Derived,
	Effect,
	type Getter,
	getter,
	type Inputs,
	type Readonlys,
	readonlys,
	Signal,
} from "@moq/signals";
import { hosted } from "../assets";
import { base64ToBytes } from "../base64";
import { arrivedAt, type Counter, type Key, type Received, Tally } from "../feedback";
import { type Failure, failureMoved, nextMedia, subscribeMedia } from "../media";
import { DELAY_MAX, type Sync, type Track } from "../sync";
import { AudioBuffer } from "./buffer";
import { type DecoderConfig, frameDuration, type PlaybackIdentity, packetDuration, playbackIdentity } from "./config";
import { Handover } from "./handover";
import { bound, maxDelay, ringHold, target } from "./latency";
import { Order } from "./order";
// A blob: URL, or a hosted file when assets() is set; see vite-plugin-worklet.
import RenderWorklet from "./render-worklet.ts?worklet";
import type { Source } from "./source";
import { supported } from "./supported";
import { Terminal } from "./terminal";

type FrameRead = Extract<Container.Read, { kind: "frame" }>;

// Ranges this close are adjacent: the tolerance for float precision.
const ADJACENT = Duration.millis(1);

type Queued = {
	// In microseconds.
	newest: number | undefined;
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

	// The playback timestamp sounding at the speaker, as of the worklet's last report.
	timestamp: Signal<Duration | undefined>;

	// Whether the audio buffer is stalled (waiting to fill). A silence the publisher declared, such as
	// a push-to-talk mute, is not a stall.
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
	/** How many times playback skipped ahead, dropping buffered audio rather than playing it faster. */
	skips: number;
	/** Media those skips dropped unplayed, less declared silence. */
	trimmed: Duration;
	/** Media the time stretch removed by playing faster. */
	accelerated: Duration;
	/** Frames that arrived after their playout time. */
	late: number;
	/** Renditions that failed to decode. */
	errors: number;
	/** Times playback stalled waiting for audio once it had played. */
	stalls: number;
	/** Total time spent in finished stalls. */
	stalled: Duration;
	/** The newest received media timestamp and the wall time it arrived. */
	arrival?: { timestamp: Duration; wall: Wall };
}

// The running totals before anything is counted.
const EMPTY: Stats = {
	bytesReceived: 0,
	underruns: 0,
	skips: 0,
	trimmed: Duration.ZERO,
	accelerated: Duration.ZERO,
	late: 0,
	errors: 0,
	stalls: 0,
	stalled: Duration.ZERO,
};

// What a decode attempt reads, and where its counts go.
type Attempt = { config: DecoderConfig; failure: Signal<Error | undefined>; path: Moq.Path.Valid; key: Key };

// The newest container consumer, the path it reads, and its codec, whose estimate a replacement
// continues.
type Consumed = { path: Moq.Path.Valid; codec: string; consumer: Container.Consumer };

// What the decoder does with the selected rendition. Only a running rendition counts a stall.
type Phase =
	| { kind: "stopped" }
	// Decoding the rendition at `key` into the ring, which an underrun or a stall is counted against.
	| { kind: "running"; key: Key; consumer?: Consumed }
	// The source failed to decode, and stays failed until another source is selected.
	| { kind: "failed"; failure: Failure };

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
	// Audio's hold on the clock: its target, startup span, and arrivals.
	readonly #track: Track;

	readonly #out: DecoderOutput = {
		error: new Signal<Error | undefined>(undefined),
		context: new Signal<AudioContext | undefined>(undefined),
		root: new Signal<AudioNode | undefined>(undefined),
		sampleRate: new Signal<number | undefined>(undefined),
		stats: new Signal<Stats | undefined>(undefined),
		timestamp: new Signal<Duration | undefined>(undefined),
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
	#measured = new Signal<Duration | undefined>(undefined);

	// The subscription's max delay: the shared budget, raised to the delay plus the stretch.
	#subscribeMaxDelay = new Signal<Duration>(Duration.ZERO);
	// The container consumer's delay: the playout delay to start at and the shared budget. Its warmup
	// reaches the start further back, to the media the decoder primes on.
	#delay = new Signal<Moq.Track.Delay>({});

	// The codec's frame duration: the catalog constant, refined by each frame's own duration.
	#frame = new Signal<Duration | undefined>(undefined);

	// Which subscription the ring's buffered samples came from. See #runDecoder.
	#handover = new Handover();

	// The broadcast instance the ring's timeline belongs to. See #runDecoder.
	#instance?: Moq.Broadcast.Consumer["closed"];

	#signals = new Effect();
	#phase = new Signal<Phase>({ kind: "stopped" });
	readonly #running: Computed<boolean>;

	// What each rendition delivered, for the feedback a viewer reports.
	#tally = new Tally();

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
			const ring = effect.get(this.#ring);
			return target({
				measured,
				advertised: effect.get(this.source.out.jitter),
				frame: effect.get(this.#frame),
				delay: delay !== undefined ? Duration.millis(delay) : undefined,
				// Audio is placed on the clock where it is heard, so video waits for the output too.
				output: ring ? effect.get(ring.output) : undefined,
			});
		});
		const startup = this.#signals.computed((effect) => {
			if (!effect.get(this.in.enabled)) return undefined;
			if (effect.get(this.source.pending)) return null;
			if (!effect.get(this.source.out.config)) return undefined;
			const span = effect.get(this.#decodeBuffered)[0];
			if (!span) return null;
			// Deep targets stay encoded, so count the audio queued past the decoded span. Sum it across
			// holes: a lost packet must not hold startup, and the missing audio is not counted as buffered.
			let queued = Duration.ZERO;
			for (const range of effect.get(this.#queued) ?? []) {
				queued = queued.add(range.end.saturatingSub(range.start.max(span.end)));
			}
			// The ring holds the delay less the output latency, which the speaker covers: the opening
			// sounds that much after it renders, so it comes due then rather than being skipped.
			const delay = effect.get(this.sync.out.delay);
			const ring = effect.get(this.#ring);
			if (ring) effect.get(ring.output);
			const held = ring ? ring.held(delay) : delay;
			return {
				start: span.start,
				end: span.end.add(queued.min(ringHold(held))),
				lead: delay.saturatingSub(held),
			};
		});
		this.#track = this.sync.track({ target: playout, startup });
		this.#signals.cleanup(() => this.#track.close());
		this.#identity = this.#signals.computed((effect) => {
			const config = effect.get(this.source.out.config);
			return config ? playbackIdentity(config) : undefined;
		});
		this.#running = this.#signals.computed((effect) => effect.get(this.#phase).kind === "running");
		this.#idle = this.#signals.computed((effect) => {
			if (effect.get(this.source.out.config) !== undefined) return false;
			const ring = effect.get(this.#ring);
			return effect.get(this.#out.stalled) || (!!ring && effect.get(ring.silent));
		});

		this.#signals.run(this.#runSubscribeMaxDelay.bind(this));
		this.#signals.run(this.#runShape.bind(this));
		this.#signals.run(this.#runWorklet.bind(this));
		this.#signals.run(this.#runEnabled.bind(this));
		this.#signals.run(this.#runLatency.bind(this));
		this.#signals.run(this.#runDecoder.bind(this));
		this.#signals.run(this.#runStalls.bind(this));
	}

	/**
	 * What this decoder received and played of each rendition in the catalog of the broadcast at
	 * `path`, keyed by rendition ID, for the feedback a viewer reports.
	 * @internal
	 */
	feedback(path: Moq.Path.Valid): Record<string, Received> {
		return this.#tally.report(path);
	}

	// Count into a rendition's feedback and the decoder's running totals.
	#count(key: Key | undefined, counts: Partial<Record<Counter, number>>, totals?: (stats: Stats) => Partial<Stats>) {
		if (key) this.#tally.count(key, counts);
		if (totals) this.#out.stats.update((current) => ({ ...(current ?? EMPTY), ...totals(current ?? EMPTY) }));
	}

	// Count a received frame and whether it arrived too late for playout.
	#receive(key: Key, frame: Container.ReceivedFrame, timestamp: Duration, late: boolean): void {
		// How far the playhead trails what arrived, decoded or not, is the level the ring steers on.
		this.#ring.peek()?.received(timestamp.add(this.#frame.peek() ?? Duration.ZERO));
		const bytes = frame.payload.byteLength;
		const arrived = arrivedAt(frame.arrival);
		this.#tally.arrive(key, timestamp, arrived);
		this.#count(key, { frames: 1, bytes, late: late ? 1 : 0 }, (stats) => ({
			bytesReceived: stats.bytesReceived + bytes,
			late: stats.late + (late ? 1 : 0),
			arrival: { timestamp, wall: arrived },
		}));
	}

	// Count each stall once audio has played, and how long it lasted. Each run of the decoder waits
	// for its first audio, which is a startup, not a stall; a rendition swap keeps the run going.
	#runStalls(effect: Effect): void {
		if (!effect.get(this.#running)) return;

		let played = false;
		effect.run((inner) => {
			if (!inner.get(this.#out.stalled)) {
				played = true;
				return;
			}
			const phase = this.#phase.peek();
			if (!played || phase.kind !== "running") return;

			const end = this.#tally.stall(phase.key);
			this.#count(undefined, {}, (stats) => ({ stalls: stats.stalls + 1 }));
			inner.cleanup(() => {
				const duration = end();
				this.#count(undefined, {}, (stats) => ({ stalled: stats.stalled.add(Duration.millis(duration)) }));
			});
		});
	}

	// The subscription reaches the stretch past the delay, so audio the ring could still catch up on
	// arrives. A group the relay expires is never observed, so a zero stretch, which cuts the
	// subscription to the target, leaves the estimate rising one bucket at a time. The container
	// consumer drops aged media at the same max delay, since it observes each frame before applying
	// it, and carves out the warmup the subscription adds once it has warmed, as Rust's does. It waits
	// on a missing group only for the delay plus the lookahead: past that the group would play late,
	// and a reordered backlog's holes waited out to the max would read as jitter to the estimator.
	// See doc/concept/audio-jitter.md.
	#runSubscribeMaxDelay(effect: Effect): void {
		const budget = this.#budget(effect);
		const delay = budget.delay;
		const max = maxDelay(budget);
		const warmup = Duration.millis(effect.get(this.source.out.config)?.warmup ?? 0).min(DELAY_MAX);
		this.#delay.set({ min: delay, max });
		this.#subscribeMaxDelay.set(max.add(warmup));
	}

	// The terms of the subscription's max delay: the shared delay and max delay, and the stretch.
	#budget(effect: Effect): { delay: Duration; maxDelay: Duration; stretch: Duration } {
		return {
			delay: effect.get(this.sync.out.delay),
			maxDelay: effect.get(this.sync.out.maxDelay),
			stretch: effect.get(this.sync.in.stretch),
		};
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
				RenderWorklet(hosted.url).then(async (url) => {
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
				clock: this.sync,
				playing: this.sync.playing,
			});
			effect.cleanup(() => ring.close());
			// An end read before the ring existed, such as a cold join during a mute, still declares
			// the silence.
			const end = this.#terminal.end;
			if (end !== undefined) ring.end(end - this.#terminal.delay);
			effect.set(this.#ring, ring);

			// Mirror ring state (timestamp/stalled) onto our public signals, until the ring closes.
			effect.run((inner) => {
				const ts = Duration.micros(inner.get(ring.timestamp));
				this.#out.timestamp.set(ts);
				this.#trimDecodeBuffered(ts);
			});
			effect.cleanup(() => this.#out.timestamp.set(undefined));
			effect.run((inner) => {
				this.#out.stalled.set(inner.get(ring.stalled) && !inner.get(ring.silent));
			});
			effect.cleanup(() => this.#out.stalled.set(true));
			// Each ring counts from zero, so add only what this one counted to the cumulative stats.
			let counted = 0;
			effect.run((inner) => {
				const underruns = inner.get(ring.underruns);
				const added = underruns - counted;
				counted = underruns;
				if (added <= 0) return;
				const phase = this.#phase.peek();
				const key = phase.kind === "running" ? phase.key : undefined;
				this.#count(key, { underruns: added }, (stats) => ({ underruns: stats.underruns + added }));
			});
			let skipped = 0;
			effect.run((inner) => {
				const skips = inner.get(ring.skips);
				const added = skips - skipped;
				skipped = skips;
				if (added <= 0) return;
				this.#count(undefined, {}, (stats) => ({ skips: stats.skips + added }));
			});
			// Each ring's totals start at zero too, so carry each forward by its rise.
			let trimmed = Duration.ZERO;
			let accelerated = Duration.ZERO;
			effect.run((inner) => {
				const total = inner.get(ring.trimmed);
				const added = total.sub(trimmed);
				trimmed = total;
				if (added.gt(Duration.ZERO))
					this.#count(undefined, {}, (stats) => ({ trimmed: stats.trimmed.add(added) }));
			});
			effect.run((inner) => {
				const total = inner.get(ring.accelerated);
				const added = total.sub(accelerated);
				accelerated = total;
				if (added.gt(Duration.ZERO)) {
					this.#count(undefined, {}, (stats) => ({ accelerated: stats.accelerated.add(added) }));
				}
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

		// The ring plays onto the shared clock, slowing down onto a rise and speeding up onto a fall,
		// and skips only audio trailing it past what the subscription still delivers. It holds the
		// delay less the output latency, which moves its target even under a fixed delay.
		effect.get(ring.output);
		const budget = this.#budget(effect);
		ring.setLatency(budget.delay, bound(budget));
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
		const phase = this.#phase.peek();
		const active =
			broadcast && track && identity && (running || phase.kind === "failed")
				? broadcast.relativeBroadcast(effect, identity.broadcast, identity.instance)
				: undefined;
		const key = identity && JSON.stringify(identity);

		// A failure belongs to its source: selecting another clears it, even while muted or paused.
		if (phase.kind === "failed") {
			const selected = broadcast ? effect.get(broadcast.out.active)?.closed : undefined;
			if (failureMoved(phase.failure, { broadcast: selected, media: active?.closed, track, identity: key })) {
				this.#phase.set({ kind: "stopped" });
				this.#out.error.set(undefined);
			}
		}

		// A source that already failed waits for another.
		if (!running || !broadcast || !track || !identity || !active || !key || this.#phase.peek().kind === "failed") {
			this.#retire();
			return;
		}

		const config = identity.decoder;

		// Closing this effect releases both its container reader and track subscription.
		const failure = new Signal<Error | undefined>(undefined);
		const rendition: Key = { path: broadcast.in.name.peek(), id: track };
		// A swap keeps the outgoing consumer, whose estimate the replacement may continue.
		this.#phase.set({
			kind: "running",
			key: rendition,
			consumer: phase.kind === "running" ? phase.consumer : undefined,
		});
		let attempt: Effect | undefined;
		effect.run((inner) => {
			const error = inner.get(failure);
			if (!error) return;
			this.#count(rendition, { errors: 1 }, (stats) => ({ errors: stats.errors + 1 }));
			this.#out.error.set(error);
			attempt?.close();
			this.#retire({
				broadcast: broadcast.out.active.peek()?.closed,
				media: active.closed,
				track,
				identity: key,
			});
		});

		// Another broadcast (a new name, or a republish) brings its own timeline, which a ring and a
		// clock anchored on the previous one would discard as already played.
		// Every handle to one instance shares `closed`, so it identifies the instance where the
		// handle itself does not.
		if (this.#instance !== undefined && this.#instance !== active.closed) {
			this.sync.reset();
			this.reset();
		} else if (this.#instance !== undefined && phase.kind === "stopped") {
			// The picture returns at the live edge. Re-join audio to that clock immediately,
			// rather than stretching the retained ring's lag back over several seconds. A
			// running rendition swap keeps its seamless handover.
			this.reset();
		}
		this.#instance = active.closed;

		// The ring outlives this effect (it's keyed on the sample rate, channel count, and mode), so a
		// replacement subscription on the same broadcast (a rendition swap, or a fallback that stays
		// running) inherits whatever its predecessor decoded; a return from an absence was reset above.
		// Samples are timestamp indexed, so the replacement overwrites the slots it lands on, but a
		// publisher writing ahead of real-time leaves seconds of tail beyond them. Drop that once the
		// replacement's first frame says where it starts.
		this.#handover.opened();

		effect.run((run) => {
			attempt = run;
			const sub = subscribeMedia(run, {
				broadcast: active,
				track: Catalog.track(track, identity),
				priority: Catalog.PRIORITY.audio,
				delay: new Derived([this.sync.out.delay, this.#subscribeMaxDelay], (min, max) => ({ min, max })),
			});
			if (!sub) {
				this.#retire();
				return;
			}

			// A config the probe accepted can still fail to parse (a malformed init segment or Opus
			// header). Report it like a decode failure, which also retires the outgoing estimate.
			const props: Attempt = { config, failure, path: active.path, key: rendition };
			spawnFatal(run, failure, async () => {
				if (config.container.kind === "cmaf") await this.#runCmafDecoder(run, sub, props);
				else this.#runLegacyDecoder(run, sub, props);
			});
		});
	}

	#runLegacyDecoder(effect: Effect, sub: Moq.Track.Subscriber, { config, failure, path, key }: Attempt): void {
		const preSkip =
			config.codec === "opus" && config.description ? Util.Opus.preSkip(Util.Hex.toBytes(config.description)) : 0;
		this.#terminal.clear(
			preSkip,
			Duration.millis(config.warmup ?? 0)
				.min(DELAY_MAX)
				.asMicros(),
		);
		const format =
			config.container.kind === "loc" ? new Container.Loc.Format("audio") : new Container.Legacy.Format(config);
		// Create consumer with slightly less latency than the render worklet to avoid underflowing.
		// TODO include JITTER_UNDERHEAD
		const { consumer, read } = this.#consume(effect, sub, format, { config, path });

		spawnFatal(effect, failure, async () => {
			const loaded = await Util.Libav.polyfill(hosted.url);
			if (!loaded) return; // cancelled

			const decoder = new AudioDecoder({
				output: (data) => {
					this.#count(key, { decoded: 1 });
					this.#emit(data);
				},
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
				read.set({ newest: order.newest(reads), buffered: queuedRanges(reads, config) });
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
					continue;
				}
				const { frame } = next;

				// Classify ingress before decoder backpressure changes the delivery clock.
				const timestamp = frame.timestamp;
				const late = this.#track.received(timestamp, frame.arrival);

				const duration = packetDuration(config.codec, frame);
				if (duration !== undefined) this.#frame.set(duration);

				this.#receive(key, frame, timestamp, late);

				if (!(await play(order.push(next)))) return;
			}
		});
	}

	async #runCmafDecoder(
		effect: Effect,
		sub: Moq.Track.Subscriber,
		{ config, failure, path, key }: Attempt,
	): Promise<void> {
		if (config.container.kind !== "cmaf") return; // just to help typescript

		const abort = effect.abort;
		const Cmaf = await import("@moq/hang/cmaf");
		if (abort.aborted) return;

		const initSegment = base64ToBytes(config.container.init);
		const init = Cmaf.decodeInitSegment(initSegment);
		const opusDescription = config.description ? Util.Hex.toBytes(config.description) : init.description;
		const preSkip = config.codec === "opus" && opusDescription ? Util.Opus.preSkip(opusDescription) : 0;
		this.#terminal.clear(
			preSkip,
			Duration.millis(config.warmup ?? 0)
				.min(DELAY_MAX)
				.asMicros(),
		);
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
			const loaded = await Util.Libav.polyfill(hosted.url);
			if (!loaded) return; // cancelled

			const decoder = new AudioDecoder({
				output: (data) => {
					this.#count(key, { decoded: 1 });
					this.#emit(data);
				},
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
				read.set({ newest: order.newest(reads), buffered: queuedRanges(reads, config) });
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
					continue;
				}
				const { frame } = next;

				const timestamp = frame.timestamp;
				const late = this.#track.received(timestamp, frame.arrival);

				const duration = packetDuration(config.codec, frame);
				if (duration !== undefined) this.#frame.set(duration);

				this.#receive(key, frame, timestamp, late);

				if (!(await play(order.push(next)))) return;
			}
		});
	}

	// Decode released frames, resetting the decoder before the first frame of a new codec epoch and
	// only then dropping one that is out of order. The terminal sees them in decode order, so it
	// anchors on the oldest. Reports whether to keep going.
	//
	// A new epoch (a gap, or a later group after a declared end) restarts the codec, never the
	// timeline: publishers keep timestamps moving forward across a declared end, so the PCM decoded
	// before it still plays and the shared clock stays anchored.
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
		await ring.wait(frame.timestamp.asMicros(), effect.abort);
		if (effect.abort.aborted) return false;

		// A fatal decode error closes the decoder, so decoding again throws InvalidStateError out
		// of the loop. Stop instead: the error callback already reported the real failure.
		if (decoder.state === "closed") return false;
		decoder.decode(
			new EncodedAudioChunk({
				// WebCodecs audio chunks are key even inside a MoQ group.
				type: "key",
				data: frame.payload,
				timestamp: frame.timestamp.asMicros(),
			}),
		);
		return true;
	}

	// A buffered subscription holds its head until it runs past the floor, which playback waits for
	// anyway, or spans the window the relay serves.
	#order(): Order<FrameRead> {
		if (!this.sync.out.buffered.peek()) return new Order(undefined);
		return new Order(() => ({
			floor: this.sync.out.delay.peek().asMicros(),
			maxDelay: this.#subscribeMaxDelay.peek().asMicros(),
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
		const phase = this.#phase.peek();
		if (phase.kind !== "running") throw new Error("audio: a consumer opened outside a running rendition");
		const outgoing = phase.consumer;
		const continues = outgoing?.path === path;
		const consumer = new Container.Consumer(sub, {
			format,
			delay: this.#delay,
			wait: this.sync.out.maxDelay,
			warmup: Duration.millis(config.warmup ?? 0).min(DELAY_MAX),
			predecessor: continues ? outgoing.consumer : undefined,
		});
		effect.cleanup(() => consumer.close());
		this.#phase.set({ ...phase, consumer: { path, codec: config.codec, consumer } });

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
			// What the consumer holds has arrived too, so its newest end is how far the received audio
			// reaches, though the gate keeps it encoded.
			let arrived: Duration | undefined;
			for (const { end } of network) {
				const micro = end.asMicros();
				if (newest === undefined || micro > newest) newest = micro;
				if (arrived === undefined || end.gt(arrived)) arrived = end;
			}
			inner.set(this.#queued, Container.mergeBufferedRanges(network, pending.buffered));
			ring.queued(newest);
			if (arrived !== undefined) ring.received(arrived);
		});

		// Feed the arrival estimate into the playout target. It outlives this consumer, so the target
		// holds until the replacement takes over; see #retire.
		effect.run((inner) => this.#measured.set(inner.get(consumer.spread)));
		return { consumer, read };
	}

	// Leave the running phase, failed or stopped; a stop keeps an earlier failure. No consumer
	// replaces the outgoing one, so its estimate stops holding the buffer open.
	#retire(failure?: Failure): void {
		if (failure) this.#phase.set({ kind: "failed", failure });
		else if (this.#phase.peek().kind === "running") this.#phase.set({ kind: "stopped" });
		this.#measured.set(undefined);
		// Its arrivals stop holding the clock's reference.
		this.#track.reset();
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
		const decoded = sample.timestamp + (sample.numberOfFrames / sample.sampleRate) * 1_000_000;
		let landed: number | undefined;
		try {
			landed = this.#place(sample);
		} finally {
			this.#ring.peek()?.decoded(decoded, landed);
		}
	}

	// Trim the output and write it to the ring, returning where the PCM written ends.
	// Microseconds, as the ring takes them.
	#place(sample: AudioData): number | undefined {
		const { timestamp, frameOffset, frames } = this.#terminal.span(sample);
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
		const durationMicro = (frames / sample.sampleRate) * 1_000_000;
		const start = Duration.micros(timestamp);
		const end = Duration.micros(timestamp + durationMicro);

		// A new subscription has taken over the timeline: drop the previous one's write-ahead tail
		// rather than letting it play out after this frame. See #runDecoder.
		if (this.#handover.takeover()) {
			ring.truncate(timestamp);
			this.#truncateDecodeBuffered(start);
		}

		// Add to decode buffer
		this.#addDecodeBuffered(start, end);

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
		return timestamp + durationMicro;
	}

	#addDecodeBuffered(start: Duration, end: Duration): void {
		if (start.gt(end)) return;

		this.#decodeBuffered.mutate((current) => {
			for (const range of current) {
				// Extend range if new sample overlaps or is adjacent (1ms tolerance for float precision)
				if (start.le(range.end.add(ADJACENT)) && end.ge(range.start)) {
					range.start = range.start.min(start);
					range.end = range.end.max(end);
					return;
				}
			}

			current.push({ start, end });
			current.sort((a, b) => a.start.cmp(b.start));
		});
	}

	// Drop reported decode ranges at or after `timestamp`, mirroring a ring truncation.
	#truncateDecodeBuffered(timestamp: Duration): void {
		this.#decodeBuffered.mutate((current) => {
			while (current.length > 0 && current[current.length - 1].start.ge(timestamp)) current.pop();
			const last = current[current.length - 1];
			if (last?.end.gt(timestamp)) last.end = timestamp;
		});
	}

	#trimDecodeBuffered(timestamp: Duration): void {
		this.#decodeBuffered.mutate((current) => {
			while (current.length > 0) {
				if (current[0].end.ge(timestamp)) {
					current[0].start = current[0].start.max(timestamp);
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

	// Apply an ordered container read before handling it, reporting whether a codec epoch ended: a
	// gap, or a later group after a declared end. The ring learns where a declared silence starts and
	// that the next utterance follows it; the timeline and the shared clock carry on through both.
	#onRead(read: Container.Read): boolean {
		// Decoded audio stops a pre-skip short of a declared end when the publisher left its codec's
		// lookahead unflushed, so the ring takes the end that early.
		if (read.kind === "end") this.#ring.peek()?.end(read.end.asMicros() - this.#terminal.delay);
		if (!this.#terminal.update(read)) return false;
		if (read.kind === "frame") this.#ring.peek()?.next();
		return true;
	}

	/** Stop and release everything this holds. */
	close() {
		this.#signals.close();
	}

	/** Whether the WebCodecs audio decoder can play this config. */
	static supported = supported;
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
		const start = frame.timestamp;
		const duration = packetDuration(config.codec, frame) ?? frameDuration(config) ?? Duration.ZERO;
		const end = start.add(duration);
		const last = ranges.at(-1);
		if (last && start.le(last.end.add(ADJACENT))) last.end = last.end.max(end);
		else ranges.push({ start, end });
	}
	return ranges;
}

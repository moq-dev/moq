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
	Once,
	type Readonlys,
	readonlys,
	Signal,
} from "@moq/signals";
import type { Broadcast } from "../broadcast";
import { errors } from "../internal";
import { Estimator, type Order } from "../order";
import type { AudioFrame, Capture, Format } from "./capture";
import { Gain } from "./gain";
import { Resampler } from "./resampler";
import type { CodecMime, Kind } from "./types";
import { sourceKind } from "./types";

const OPUS_BITRATE_PER_CHANNEL = 32_000;
const OPUS_FRAME_DURATION = Milli(20);
// The only frame durations libopus (and so WebCodecs) will encode, in ms.
const OPUS_FRAME_DURATIONS = [2.5, 5, 10, 20, 40, 60];
const AAC_BITRATE_PER_CHANNEL = 64_000;
const AAC_FRAME_SAMPLES = 1024; // AAC-LC encodes a fixed 1024 samples per frame.
// Long enough that a volume change doesn't click, short enough that a mute is silent almost at once.
const FADE = Milli(50);

// The WebCodecs/MP4 codec string for AAC-LC. "aac" is our user-facing shorthand.
const AAC_CODEC = "mp4a.40.2";

import { Micro, Milli } from "@moq/clock";
import { Framer } from "./framer";

/**
 * Selects the audio codec and its encoder settings. Either the bare codec name (all defaults) or an
 * object with the mime plus tuning knobs.
 */
export type Codec = Opus | Aac;

/** Opus, by name with every default or as a config. */
export type Opus = "opus" | OpusConfig;
/** AAC, by name with every default or as a config. */
export type Aac = "aac" | AacConfig;

/**
 * AAC encoder settings. AAC-LC has a fixed 1024-sample frame and no real-time tuning knobs, so
 * bitrate is the only thing to configure.
 */
export type AacConfig = {
	mime: "aac";

	bitrate?: number; // bits/sec, defaults to channelCount * 64kbps
};

/**
 * Opus encoder settings. bitrate and frameDuration also shape the catalog (decoders need them); the
 * rest are encode-only knobs that map directly to the matching OpusEncoderConfig fields:
 * https://developer.mozilla.org/en-US/docs/Web/API/AudioEncoder/configure#opus
 */
export type OpusConfig = {
	mime: "opus";

	bitrate?: number; // bits/sec, defaults to channelCount * 32kbps
	// The type carries the unit (ms): build with Milli(20). Opus takes exactly 2.5, 5, 10, 20,
	// 40, or 60 ms, and defaults to 20.
	frameDuration?: Milli;
	complexity?: number; // 0-10, higher is better quality but more CPU
	packetlossperc?: number; // 0-100, expected loss the encoder optimizes for
	useinbandfec?: boolean; // in-band forward error correction
};

/** Cumulative encoder output totals, measured from the chunks the encoder produces. */
export interface Stats {
	/** Total frames encoded while serving. Monotonic; diff over an interval for a frame rate. */
	frames: number;

	/** Total bytes encoded while serving. Monotonic; diff over an interval for an upload bitrate. */
	bytes: number;
}

/** Signals the encoder reads. */
export type EncoderInput = {
	// Whether to encode this rendition. Defaults to true. When false it stops encoding, ending the epoch,
	// but stays in the catalog with `enabled: false` and its last config, and stays registered so a
	// subscriber still gets an idle track.
	enabled: Getter<boolean>;

	// The broadcast to register the rendition on. Undefined resolves the config but has nowhere to publish.
	broadcast: Getter<Broadcast | undefined>;

	// The capture supplying PCM. Shared: one capture feeds any number of renditions, so build it
	// yourself and pass the same instance to each.
	capture: Getter<Capture | undefined>;

	// The connection's bandwidth allocator. Audio reserves its configured bitrate so
	// video's share is honest, and ignores the grant (Opus is a fixed rate today).
	bandwidth: Getter<Moq.Bandwidth.Handle | undefined>;
};

/** Constructor options: the wired inputs plus the live-editable tuning knobs. */
export type EncoderProps = Inputs<EncoderInput> & {
	// User tuning knobs. Seed a value or wire a Signal; also live-editable via the matching field.
	volume?: number | Signal<number>;

	// Codec selection plus encoder settings. Defaults to "opus".
	codec?: Codec | Signal<Codec>;

	// The minimum audio carried by each group. Defaults to 20 ms; zero puts every frame in its own group.
	groupDuration?: Milli | Signal<Milli>;
};

/** Signals the encoder derives. */
export type EncoderOutput = {
	/** The encoder failure, cleared only by a new capture source or format, or new encoder settings. */
	error: Signal<Error | undefined>;
	// The catalog config published for this rendition, `enabled: false` while disabled, or undefined
	// while there's no capture.
	catalog: Signal<Catalog.AudioConfig | undefined>;
	// The head of the capture graph, so callers can tap the raw capture. Volume is applied to the
	// PCM rather than in the graph, so this is pre-gain. Undefined for a source that isn't a track.
	root: Signal<AudioNode | undefined>;
	// True when a subscriber is attached and we're encoding.
	active: Signal<boolean>;
	// Cumulative output totals (frames, bytes) measured while serving.
	stats: Signal<Stats>;
};

// One configured encode chain. Rebuilt whenever the resolved config changes; the capture read loop
// pushes into whichever one is current, so a codec change never interrupts the source.
type Pipeline = {
	channelCount: number;
	push(frame: AudioFrame): void;
};

/**
 * A single audio rendition encoder.
 *
 * Registers itself on the {@link Broadcast} under {@link name} (via `broadcast.audio(name)`), pumps PCM
 * off the source via a {@link Capture}, and encodes it only while a subscriber is attached (the demand
 * gate). Rename by constructing a new encoder; the name is not a signal.
 */
export class Encoder {
	/** The full track name of this rendition, e.g. `"audio/data"`. */
	readonly name: string;

	/** The wired inputs, read-only. */
	readonly in: Readonlys<EncoderInput>;

	/** Linear gain applied before encoding, where 1 is unity. Each change ramps over 50 ms, so a mute is silent once it passes. */
	volume: Signal<number>;
	/** The live-editable codec selection plus its encoder settings. */
	codec: Signal<Codec>;
	/**
	 * The minimum timestamp span before a frame opens the next group, closing the previous one. Defaults to 20 ms; zero puts every frame in its own group. A longer group costs the
	 * relay fewer streams but makes loss coarser: a viewer that falls behind skips a whole group. A
	 * negative or non-finite duration refuses the rendition.
	 */
	groupDuration: Signal<Milli>;

	/**
	 * The capture supplying this rendition, or undefined while none is wired.
	 *
	 * A snapshot, for reaching the shared capture format knobs (`audio.capture?.sampleRate`). Read
	 * {@link in}.capture through an effect instead when you need to react to it being swapped.
	 */
	get capture(): Capture | undefined {
		return this.in.capture.peek();
	}

	// The exact encoder settings and probed decoder description advertised before demand arrives.
	#config = new Signal<(Resolved & { encoder: AudioEncoderConfig }) | undefined>(undefined);

	// The last Opus probe: the encoder settings it ran with, serialized, and the description it
	// produced. Kept across a disable so re-enabling with the same capture format and settings
	// republishes at once instead of probing again.
	#probed?: { key: string; description?: Catalog.Hex };

	readonly #out: EncoderOutput = {
		error: new Signal<Error | undefined>(undefined),
		catalog: new Signal<Catalog.AudioConfig | undefined>(undefined),
		root: new Signal<AudioNode | undefined>(undefined),
		active: new Signal<boolean>(false),
		stats: new Signal<Stats>({ frames: 0, bytes: 0 }),
	};
	/** The derived outputs, read-only. */
	readonly out = readonlys(this.#out);

	// The encode chain currently publishing, or undefined while nothing is. The read loop pushes
	// into this; frames that arrive while it's undefined are dropped, which the framer treats as a
	// discontinuity and re-anchors on.
	#pipeline: Pipeline | undefined;

	// The current subscription's track and the producer writing into it, or undefined without demand.
	// `start` is the open group's first timestamp, undefined until a frame opens one.
	#live: { track: Moq.Track.Producer; producer: Container.Legacy.Producer; start?: Micro } | undefined;

	// Where the next frame submitted to the AudioEncoder starts, i.e. the exclusive end of the
	// newest one, where an epoch's discontinuity marker goes. Cleared once the marker is written.
	#next: Micro | undefined;

	// The newest epoch's marker. The AudioEncoder outlives a demand gap too brief to skip a frame,
	// so chunks it still held when demand disappeared surface after the resume; they sit below the
	// marker and are dropped.
	#floor: Micro | undefined;

	// The last valid group duration, which #runConfig validates.
	#groupDuration = Micro(20_000);

	// How many resolution runs threw for their current inputs (e.g. invalid codec settings), so no
	// config is coming until one reruns.
	#failures = new Signal(0);

	/**
	 * @internal Whether the catalog config resolved, or can't until something outside the encoder
	 * changes: a failed encoder or capture, invalid codec settings, or a capture waiting on the page's
	 * first gesture. `<moq-publish>` holds its first announce until this is set.
	 */
	readonly settled: Computed<boolean>;

	// Each registration keeps its terminal failure even after the public error clears.
	#failure = new Signal(new Once<Error>());
	// The capture inputs #runCaptureRecovery last saw, carried across a gap in the source or format.
	#seen?: { capture?: Capture; source?: unknown; format?: Format };
	#signals = new Effect();
	#estimator = new Estimator();
	// The estimator's delay, republished whenever it rises.
	#estimate = new Signal<Estimator["estimate"]>({});
	// What this rendition advertised on each broadcast, keyed by the broadcast or, with none, the
	// encoder. A broadcast keeps a rendition's track when it re-registers, so returning to one
	// continues its bitrate maximum and last config, while another broadcast starts its own.
	#advertised = new WeakMap<object, { bitrate: number; last?: Catalog.AudioConfig }>();
	// Whether the rendition was disabled since a config last resolved, so it keeps advertising its last
	// config as disabled until the re-enabled capture resolves a new one. A capture that never reopens
	// leaves it disabled, which is accurate: no frames are coming.
	#paused = false;

	constructor(name: string, props?: EncoderProps) {
		// `source` moved to Audio.Capture, which renditions share. TypeScript catches this, but a
		// plain JS caller would otherwise get an encoder with nothing attached: no catalog, no
		// audio, and nothing to explain why.
		if (props && "source" in props) {
			throw new Error("Audio.Encoder: `source` moved to Audio.Capture; construct one and pass `capture`");
		}

		this.name = name;
		this.in = {
			enabled: getter(props?.enabled ?? true),
			broadcast: getter(props?.broadcast),
			capture: getter(props?.capture),
			bandwidth: getter(props?.bandwidth),
		};
		this.volume = Signal.from(props?.volume ?? 1);
		this.codec = Signal.from<Codec>(props?.codec ?? "opus");
		this.groupDuration = Signal.from(props?.groupDuration ?? Milli(20));

		// Only the capture graph has a node to expose.
		this.#signals.run((effect) => {
			const capture = effect.get(this.in.capture);
			if (!capture) return;
			effect.proxy(this.#out.root, capture.out.root);
		});

		this.settled = this.#signals.computed((effect) => {
			if (effect.get(this.#out.catalog) !== undefined) return true;
			if (effect.get(this.#out.error) !== undefined || effect.get(this.#failures) > 0) return true;
			const capture = effect.get(this.in.capture);
			return capture !== undefined && !!effect.get(capture.blocked);
		});

		this.#signals.run(this.#runCapture.bind(this));

		// Every step that resolves the config counts a throw as a failure, so a bad input settles the
		// gate instead of holding the announce forever.
		for (const run of [this.#runConfig, this.#runCatalog]) {
			this.#signals.run((effect) => {
				try {
					run.call(this, effect);
				} catch (err) {
					if (run === this.#runConfig) this.#config.set(undefined);
					this.#fail(effect);
					throw err;
				}
			});
		}
		this.#signals.run(this.#runSettingsRecovery.bind(this));
		this.#signals.run(this.#runCaptureRecovery.bind(this));
		this.#signals.run(this.#runRegister.bind(this));
	}

	// Pump PCM off the capture into whatever is currently publishing, applying the volume knobs on
	// the way through. Tied to the capture's lifetime rather than the encoder's, so reconfiguring
	// never has to reacquire the stream, which for a decoded file would be fatal.
	#runCapture(effect: Effect): void {
		const capture = effect.get(this.in.capture);
		if (!capture) return;

		const fanout = effect.get(capture.out.frames);
		if (!fanout) return;

		// Our own stream off the shared capture, so another rendition reading slowly can't take
		// frames from this one.
		const reader = fanout.subscribe(effect).getReader();
		effect.cleanup(() => {
			reader.cancel().catch(() => {});
		});

		const gain = new Gain(this.volume.peek());

		effect.spawn(async () => {
			for (;;) {
				const next = await effect.race(reader.read());
				if (!next?.value) break;

				const format = capture.out.format.peek();
				if (!format) continue;

				// Every rendition shares the captured frame, so gain returns a copy rather than
				// scaling in place; muting one rendition must not silence the rest.
				gain.set(this.volume.peek(), FADE);
				const frame = gain.apply(next.value, format.sampleRate);

				// The config rebuilds when the channel count moves, so skip anything that arrives
				// mid-swap rather than framing it wrong.
				const pipeline = this.#pipeline;
				if (pipeline && pipeline.channelCount === frame.channels.length) {
					try {
						pipeline.push(frame);
					} catch (error) {
						this.#abort(error);
					}
				}
			}
		});
	}

	// Any notification of the encoder settings re-arms a failure, including a setting mutated in place.
	#runSettingsRecovery(effect: Effect): void {
		effect.get(this.codec);
		this.#rearm();
	}

	// Read only capture inputs here, never bandwidth grants or the error itself; demand is not a retry.
	// A gap in the source or format (a capture disabled or interrupted) keeps the last ones, so only a
	// different capture re-arms a failure, such as the new track a re-enabled microphone opens.
	#runCaptureRecovery(effect: Effect): void {
		const capture = effect.get(this.in.capture);
		const seen = this.#seen;
		const same = seen !== undefined && seen.capture === capture;
		const source = (capture && effect.get(capture.in.source)) ?? (same ? seen.source : undefined);
		const format = (capture && effect.get(capture.out.format)) ?? (same ? seen.format : undefined);
		this.#seen = { capture, source, format };
		if (
			same &&
			seen.source === source &&
			seen.format?.sampleRate === format?.sampleRate &&
			seen.format?.channelCount === format?.channelCount
		) {
			return;
		}
		this.#rearm();
	}

	// Clear a latched failure so a fresh registration retries.
	#rearm(): void {
		if (!this.#out.error.peek()) return;
		this.#out.error.set(undefined);
		this.#floor = undefined;
		this.#failure.set(new Once<Error>());
	}

	// Register the rendition on the broadcast, publish its config, and encode only while a subscriber
	// is attached (the demand gate). Re-registers cleanly when the broadcast swaps.
	#runRegister(effect: Effect): void {
		const failure = effect.get(this.#failure);
		const broadcast = effect.get(this.in.broadcast);
		if (!broadcast) return;

		const rendition = broadcast.audio(this.name);
		errors.set(rendition, failure);
		effect.cleanup(() => rendition.close());

		// Publish the resolved config; undefined (no capture) drops it from the catalog.
		effect.proxy(rendition.config, this.out.catalog);

		// The pipeline outlives any one subscription: it is built as soon as capture runs and
		// #encode reads the live producer per frame rather than subscribing to it. Rebuilding on a
		// swap would close the AudioEncoder, which discards every chunk the codec still holds, and
		// would restart the framer mid-frame, so the output fell permanently behind its input.
		effect.run((effect) => {
			const enabled = effect.get(this.in.enabled);
			const capture = effect.get(this.in.capture);
			const format = capture ? effect.get(capture.out.format) : undefined;
			const fatal = effect.get(this.#out.error);
			const resolved = effect.get(this.#config);
			if (!enabled || !format || fatal || !resolved) return;

			this.#encode(broadcast.order, format, effect);
		});

		// Each subscription gets its own producer. Losing demand ends the epoch, as stopping the
		// pipeline does.
		effect.run((effect) => {
			const track = effect.get(rendition.track);
			if (!track) return;

			const live = {
				track,
				producer: new Container.Legacy.Producer(track, new Container.Legacy.Format("audio")),
			};
			this.#live = live;
			effect.cleanup(() => {
				this.#cut();
				if (this.#live === live) this.#live = undefined;
			});
		});

		effect.run((effect) => {
			const enabled = effect.get(this.in.enabled);
			const capture = effect.get(this.in.capture);
			const format = capture ? effect.get(capture.out.format) : undefined;
			const track = effect.get(rendition.track);
			const fatal = effect.get(this.#out.error);

			// A dead encoder can't serve anyone, so the current subscriber and every later one get
			// the real error rather than a track that stays silent.
			if (fatal) track?.abort(fatal);

			effect.set(this.#out.active, enabled && !!format && !!track && !fatal, false);
		});

		// Claim the configured bitrate so a co-resident video encoder's share is
		// honest. Wait for a bitrate so we never claim 0. The grant is ignored:
		// following it for Opus is out of scope.
		effect.run((effect) => {
			const enabled = effect.get(this.in.enabled);
			const track = effect.get(rendition.track);
			const allocator = effect.get(this.in.bandwidth);
			if (!enabled || !track || !allocator || effect.get(this.#out.error)) return;

			let reservation: Moq.Bandwidth.Reservation | undefined;
			effect.subscribe(this.#config, (config) => {
				const bitrate = config?.catalog.bitrate;
				if (bitrate === undefined) return;
				if (!reservation) reservation = allocator.reserve(track.demand(), bitrate);
				else reservation.update(bitrate);
			});
			effect.cleanup(() => reservation?.close());
		});
	}

	// Derive the encoder config from the captured format and the codec. Re-runs whenever either changes, so a
	// codec update (bitrate, frame duration) reconfigures without waiting for a channel-count change.
	//
	// Gated on `enabled` the same way the video encoder is: a disabled rendition stops resolving a
	// config, and a sample source keeps its format while muted rather than tearing down. Re-enabling
	// with the settings last probed reuses that probe, so the catalog flips back without waiting on one.
	#runConfig(effect: Effect): void {
		const groupDuration = effect.get(this.groupDuration);
		if (!Number.isFinite(groupDuration) || groupDuration < 0)
			throw new Error(`audio group duration must be a finite, non-negative number of ms: ${groupDuration}`);
		this.#groupDuration = Micro.fromMilli(groupDuration);

		const capture = effect.get(this.in.capture);
		const captured = capture ? effect.get(capture.out.format) : undefined;
		if (!effect.get(this.in.enabled) || !captured) {
			this.#config.set(undefined);
			return;
		}

		const resolved = resolve(captured, effect.get(this.codec));
		const source = capture ? effect.get(capture.in.source) : undefined;
		const kind = source ? sourceKind(source) : "auto";
		const encoderConfig = toEncoderConfig(resolved, kind, this.#opusOptions(effect));
		if (resolved.catalog.codec !== "opus") {
			this.#config.set({ ...resolved, encoder: encoderConfig });
			return;
		}

		const key = JSON.stringify(encoderConfig);
		if (this.#probed?.key === key) {
			this.#config.set(withDescription(resolved, encoderConfig, this.#probed.description));
			return;
		}

		const superseded = effect.abort;
		effect.spawn(async () => {
			try {
				await Util.Libav.polyfill();
				if (superseded.aborted) return;
				let description: Catalog.Hex | undefined;
				let failure: Error | undefined;
				const encoder = new AudioEncoder({
					output: (_frame, metadata) => {
						const source = metadata?.decoderConfig?.description;
						if (!source) return;
						const bytes = ArrayBuffer.isView(source)
							? new Uint8Array(source.buffer, source.byteOffset, source.byteLength)
							: new Uint8Array(source);
						description = Util.Hex.fromBytes(bytes);
					},
					error: (error) => {
						failure = error;
					},
				});
				effect.cleanup(() => {
					if (encoder.state !== "closed") encoder.close();
				});
				encoder.configure(encoderConfig);
				const samples = Math.round(
					(encoderConfig.sampleRate * (resolved.frameDuration ?? Micro(20_000))) / 1_000_000,
				);
				const frame = new AudioData({
					format: "f32-planar",
					sampleRate: encoderConfig.sampleRate,
					numberOfChannels: encoderConfig.numberOfChannels,
					numberOfFrames: samples,
					timestamp: 0,
					data: new Float32Array(samples * encoderConfig.numberOfChannels),
				});
				try {
					encoder.encode(frame);
				} finally {
					frame.close();
				}
				await effect.race(encoder.flush());
				if (superseded.aborted) return;
				if (failure) throw failure;
				encoder.close();
				this.#probed = { key, description };
				this.#config.set(withDescription(resolved, encoderConfig, description));
			} catch (error) {
				if (superseded.aborted) return;
				this.#config.set(undefined);
				this.#fail(effect);
				throw error;
			}
		});
	}

	#fail(effect: Effect): void {
		this.#failures.update((n) => n + 1);
		effect.cleanup(() => this.#failures.update((n) => n - 1));
	}

	// Publish the probed config before a consumer can request the demand-gated track. A
	// disabled rendition keeps its last config, since muting also releases the capture, and stays
	// disabled after re-enabling until the reopened capture reports its format.
	#runCatalog(effect: Effect): void {
		const estimate = effect.get(this.#estimate);
		const enabled = effect.get(this.in.enabled) && !effect.get(this.#out.error);
		const key = effect.get(this.in.broadcast) ?? this;
		let track = this.#advertised.get(key);
		if (!track) {
			track = { bitrate: 0 };
			this.#advertised.set(key, track);
		}
		if (!enabled) this.#paused = true;
		const config = enabled ? effect.get(this.#config)?.catalog : undefined;
		if (!config) {
			const last = this.#paused ? track.last : undefined;
			effect.set(this.#out.catalog, last && { ...last, ...estimate, enabled: false });
			return;
		}
		this.#paused = false;

		// The catalog bitrate is the most this track has been configured for, so lowering the knob
		// retunes the encoder without lowering what the catalog advertises.
		track.bitrate = Math.max(track.bitrate, config.bitrate ?? 0);
		const bitrate = track.bitrate > 0 ? Catalog.u53(track.bitrate) : undefined;
		track.last = { ...config, bitrate, ...estimate };
		effect.set(this.#out.catalog, track.last);
	}

	// Collect the encode-only Opus knobs that are set, reading the codec through the effect so the
	// encoder reconfigures when it changes. Undefined values are omitted so the browser keeps its defaults.
	#opusOptions(effect: Effect): OpusEncoderConfigExt {
		const codec = normalizeCodec(effect.get(this.codec));
		const opus: OpusEncoderConfigExt = {};
		if (codec.mime !== "opus") return opus;

		if (codec.complexity !== undefined) opus.complexity = codec.complexity;
		if (codec.packetlossperc !== undefined) opus.packetlossperc = codec.packetlossperc;
		if (codec.useinbandfec !== undefined) opus.useinbandfec = codec.useinbandfec;

		return opus;
	}

	// End the epoch with a discontinuity marker (see Container.Legacy.Producer.discontinuity) where the
	// submitted media ends, so a subscriber that stays across the break, or joins during it, doesn't
	// read the audio before it as live. Called whenever demand disappears or the pipeline stops.
	#cut(): void {
		const end = this.#next;
		this.#next = undefined;
		const live = this.#live;
		if (end === undefined || !live || live.track.closed.peek() !== undefined) return;
		this.#floor = end;
		live.producer.discontinuity(end);
		live.start = undefined;
	}

	// Encode captured audio frames into whichever track producer is live. The broadcast owns the
	// track's lifetime, so this never closes it; a fatal encoder error aborts only the live track.
	#encode(order: Order, format: Format, effect: Effect): void {
		// The parent owns the config subscription before the async codec scope is opened.
		const resolved = this.#config.peek();
		if (!resolved) return;
		// A stopped rendition stops holding the others' delay back.
		effect.cleanup(() => order.leave(this.#estimator));
		effect.spawn(async () => {
			// We're using an async polyfill temporarily for Safari support.
			await Util.Libav.polyfill();

			effect.run((effect: Effect) => {
				const aborted = effect.abort;
				const config = resolved.catalog;

				const encoderConfig = resolved.encoder;

				// WebCodecs rejects input whose rate doesn't match the encoder config outright, so
				// anything arriving at a rate the codec can't carry (a 44.1kHz file as Opus) has to
				// be converted first. A capture graph already runs at the right rate, so this is
				// usually nothing.
				const resampler =
					format.sampleRate === config.sampleRate
						? undefined
						: new Resampler({
								from: format.sampleRate,
								to: config.sampleRate,
								channels: config.numberOfChannels,
							});

				const framer = createFramer(resolved, config.sampleRate);

				const encoder = new AudioEncoder({
					output: (frame) => {
						if (frame.type !== "key") {
							throw new Error("only key frames are supported");
						}

						this.#out.stats.update((stats) => ({
							frames: stats.frames + 1,
							bytes: stats.bytes + frame.byteLength,
						}));

						const live = this.#live;
						if (!live) return;
						const timestamp = frame.timestamp as Micro;
						if (this.#floor !== undefined && timestamp < this.#floor) return;

						// Every audio frame decodes on its own, so any of them can open a group: the
						// first one at or past the minimum after the open group's start. Frames forward
						// as they are written rather than waiting for the group to fill. A dropped
						// group leaves a gap for the codec's PLC.
						const keyframe = live.start === undefined || timestamp - live.start >= this.#groupDuration;
						if (keyframe) live.start = timestamp;
						live.producer.encode(frame, timestamp, keyframe);
						if (this.#estimator.write(frame.timestamp, order)) {
							this.#estimate.set({ ...this.#estimator.estimate });
						}
					},
					error: (err) => {
						if (!aborted.aborted) this.#abort(err);
					},
				});
				// A fatal error already closed the codec, and closing it twice throws.
				effect.cleanup(() => {
					if (encoder.state !== "closed") encoder.close();
				});

				console.debug("encoding audio", encoderConfig);
				try {
					encoder.configure(encoderConfig);
				} catch (error) {
					this.#abort(error);
					return;
				}

				// Where the next frame starts if it continues the last one encoded.
				let contiguous: Micro | undefined;

				const pipeline: Pipeline = {
					channelCount: config.numberOfChannels,
					push: (captured: AudioFrame) => {
						const input = resampler ? resampler.push(captured) : captured;
						if (!input) return;

						const frames = framer.push(input);
						for (const [i, data] of frames.entries()) {
							// The demand gate. The framer still consumes every sample so its timestamps stay
							// on the capture clock, but there is nowhere to send a chunk with no subscriber.
							if (!this.#live) continue;

							// Round to whole microseconds once, here, so a chunk's timestamp and the marker
							// placed at the next frame's start agree exactly.
							const timestamp = Math.round(data.timestamp) as Micro;

							// Chrome stamps encoder output from the first input's timestamp plus the samples
							// encoded since, ignoring any later jump. Across a gap (demand, or a capture
							// discontinuity) the output would trail the capture clock by the gap, and the
							// next demand gap's marker would then sit ahead of everything encoded after it.
							// Restarting re-bases the output clock; the chunks it drops predate the gap.
							if (contiguous !== undefined && timestamp !== contiguous) {
								encoder.reset();
								encoder.configure(encoderConfig);
							}

							const joinedLength = data.channels.reduce((total, channel) => total + channel.length, 0);
							const joined = new Float32Array(joinedLength);

							data.channels.reduce((offset: number, channel: Float32Array): number => {
								joined.set(channel, offset);
								return offset + channel.length;
							}, 0);

							const frame = new AudioData({
								format: "f32-planar",
								sampleRate: config.sampleRate,
								numberOfFrames: data.channels[0].length,
								numberOfChannels: data.channels.length,
								timestamp,
								data: joined,
								transfer: [joined.buffer],
							});

							encoder.encode(frame);
							frame.close();
							// One input can complete several frames, and the framer has already advanced past all of them.
							contiguous = Math.round(frames[i + 1]?.timestamp ?? framer.next) as Micro;
							this.#next = contiguous;
						}
					},
				};

				// Publish it last: the read loop starts pushing the moment this is visible.
				this.#pipeline = pipeline;
				effect.cleanup(() => {
					if (this.#pipeline !== pipeline) return;
					this.#pipeline = undefined;
					// Whatever stopped it (disable, a format or codec change, a fatal error), the
					// timeline breaks here.
					this.#cut();
				});
			});
		});
	}

	#abort(error: unknown): void {
		if (this.#out.error.peek()) return;
		const cause = error instanceof Error ? error : new Error(String(error));
		this.#failure.peek().set(cause);
		this.#out.error.set(cause);
		this.#live?.track.abort(cause);
	}

	/** Stop and release everything this holds. */
	close() {
		this.#signals.close();
	}
}

/**
 * The encode settings a {@link Codec} resolves to against a captured PCM format.
 *
 * The catalog is a decoder hint carrying whole milliseconds, so it can only round a frame duration.
 * {@link frameDuration} keeps the exact value the encoder is configured with, which is what lets
 * Opus run at 2.5 ms.
 */
type Resolved = {
	/** The rendition config published in the catalog. */
	catalog: Catalog.AudioConfig;

	/** The exact encoded frame duration, or undefined for a codec whose frame is a fixed sample count. */
	frameDuration?: Micro;
};

/**
 * Resolve a {@link Codec} against the captured PCM format, giving what the encoder will run with
 * and the catalog rendition published alongside it.
 * @internal
 */
export function resolve(captured: Format, selected: Codec): Resolved {
	const codec = normalizeCodec(selected);

	// The catalog has to describe what the encoder emits, not what we feed it. A capture graph
	// already runs at a rate the codec supports, since Capture picks the AudioContext rate.
	// Decoded samples arrive at whatever rate the file was authored at, which Opus may not be
	// able to carry, so snap to one it can and let #encode resample into it.
	const rate = pickSampleRate(codec.mime, captured.sampleRate) ?? captured.sampleRate;

	const sampleRate = Catalog.u53(rate);
	const numberOfChannels = Catalog.u53(captured.channelCount);

	if (codec.mime === "aac") {
		return {
			catalog: {
				codec: AAC_CODEC,
				sampleRate,
				numberOfChannels,
				bitrate: Catalog.u53(codec.bitrate ?? captured.channelCount * AAC_BITRATE_PER_CHANNEL),
				container: { kind: "legacy" } as const,
				// Frames are raw (no ADTS header), so the decoder needs the AudioSpecificConfig to init.
				description: Util.Hex.fromBytes(Util.Aac.audioSpecificConfig(rate, captured.channelCount)),
			},
		};
	}

	const frameDuration = codec.frameDuration ?? OPUS_FRAME_DURATION;
	// Check here rather than letting AudioEncoder.configure throw: by then the rendition has been
	// advertised and requested, so the failure surfaces to a subscriber instead of the caller.
	if (!OPUS_FRAME_DURATIONS.includes(frameDuration)) {
		throw new Error(`opus frame duration must be ${OPUS_FRAME_DURATIONS.join("/")} ms: ${frameDuration}`);
	}

	return {
		catalog: {
			codec: "opus",
			warmup: Catalog.u53(Util.Opus.WARMUP),
			sampleRate,
			numberOfChannels,
			bitrate: Catalog.u53(codec.bitrate ?? captured.channelCount * OPUS_BITRATE_PER_CHANNEL),
			container: { kind: "legacy" } as const,
		},
		frameDuration: Micro.fromMilli(frameDuration),
	};
}

// The resolved config with the encoder settings and the probed decoder description, if the codec emitted one.
function withDescription(
	resolved: Resolved,
	encoder: AudioEncoderConfig,
	description: Catalog.Hex | undefined,
): Resolved & { encoder: AudioEncoderConfig } {
	return {
		...resolved,
		catalog: { ...resolved.catalog, ...(description === undefined ? {} : { description }) },
		encoder,
	};
}

// Build the framer for a config, given the rate the PCM actually arrives at. That's the catalog rate
// for a capture graph, but a decoded file can arrive at a rate the codec doesn't carry (44100 for
// Opus), and the framer has to count the samples we're handed rather than the ones the encoder emits.
function createFramer(resolved: Resolved, sampleRate: number): Framer {
	const config = resolved.catalog;

	// WebCodecs copies input AudioData timestamps to encoded chunks. Align those inputs to codec frames
	// because the worklet's 128-sample quanta usually do not align with Opus frame boundaries.
	if (config.codec.startsWith("mp4a")) {
		return new Framer({
			sampleRate,
			channels: config.numberOfChannels,
			size: { samples: AAC_FRAME_SAMPLES },
		});
	}

	if (config.codec !== "opus") throw new Error(`unsupported audio codec: ${config.codec}`);
	return new Framer({
		sampleRate,
		channels: config.numberOfChannels,
		size: { duration: resolved.frameDuration ?? Micro.fromMilli(OPUS_FRAME_DURATION) },
	});
}

// Resolve the bare codec shorthands to their full config object so callers can read fields uniformly.
function normalizeCodec(codec: Codec): OpusConfig | AacConfig {
	if (codec === "opus") return { mime: "opus" };
	if (codec === "aac") return { mime: "aac" };
	return codec;
}

// `application` and `signal` are in the WebCodecs spec but missing from lib.dom.d.ts.
// https://www.w3.org/TR/webcodecs-opus-codec-registration/#dom-opusencoderconfig
interface OpusEncoderConfigExt extends OpusEncoderConfig {
	application?: "voip" | "audio" | "lowdelay";
	signal?: "auto" | "voice" | "music";
}

// Opus settings implied by the audio kind. These are only defaults: any field set explicitly via
// OpusConfig (carried in opusOptions) overrides them, so a caller can always opt out. "auto" leaves
// every knob to the browser.
function opusKindDefaults(kind: Kind): OpusEncoderConfigExt {
	switch (kind) {
		case "voice":
			return { application: "voip", signal: "voice" };
		case "music":
			return { application: "audio", signal: "music" };
		default:
			return {};
	}
}

/**
 * Build the WebCodecs encoder config from the catalog (decoder) config, a Kind hint, and any
 * Opus-only knobs. Those knobs are kept out of the catalog since they only affect encoding. AAC has
 * no such knobs, so it just uses the shared base fields (codec/sampleRate/channels/bitrate).
 * @internal
 */
export function toEncoderConfig(resolved: Resolved, kind: Kind, opusOptions: OpusEncoderConfigExt): AudioEncoderConfig {
	const config = resolved.catalog;
	const encoderConfig: AudioEncoderConfig = {
		codec: config.codec,
		sampleRate: config.sampleRate,
		numberOfChannels: config.numberOfChannels,
		bitrate: config.bitrate,
	};

	if (config.codec.startsWith("mp4a")) {
		// Pin raw AAC: the catalog carries a synthesized AudioSpecificConfig, which is only valid for
		// raw frames. An ADTS default would make the frames self-describing and that description wrong.
		encoderConfig.aac = { format: "aac" };
	}

	if (config.codec === "opus") {
		// Start from the kind's defaults, then let explicit opusOptions win (undefined knobs were
		// already dropped upstream, so the spread only overrides what the caller actually set).
		const opus: OpusEncoderConfigExt = { ...opusKindDefaults(kind), ...opusOptions };

		// The exact duration, not the catalog's rounded jitter hint: WebCodecs rejects anything but
		// 2.5/5/10/20/40/60 ms, so 2.5 has to arrive as 2500 µs rather than 3000.
		if (resolved.frameDuration !== undefined) {
			opus.frameDuration = resolved.frameDuration;
		}

		if (Object.keys(opus).length > 0) {
			encoderConfig.opus = opus;
		}
	}

	return encoderConfig;
}

/**
 * Snap a rate to one the codec can actually encode at.
 *
 * The capture runs at whatever suits the device, and several renditions may share it, so each
 * rendition converts on its own. WebCodecs rejects input whose rate doesn't match the configured
 * one rather than converting for us, which is what #encode's resampler is for.
 */
function pickSampleRate(mime: CodecMime, requested: number | undefined): number | undefined {
	// Treat a nonsense rate as unknown, rather than snapping it to the codec's floor (7350Hz for AAC).
	const rate = requested !== undefined && Number.isFinite(requested) && requested > 0 ? requested : undefined;

	// Opus only decodes at a handful of rates, and 44.1kHz is not one of them.
	if (mime === "opus") return Util.Opus.pickRate(rate ?? Util.Opus.DEFAULT_SAMPLE_RATE);

	// The AAC table includes 44100, so an unknown rate can fall through to whatever we captured.
	return rate !== undefined ? Util.Aac.pickRate(rate) : undefined;
}

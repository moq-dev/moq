import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Util from "@moq/hang/util";
import type * as Moq from "@moq/net";
import { Time } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";
import type { AudioFrame, Capture, Format } from "./capture";
import { Gain } from "./gain";
import { Resampler } from "./resampler";
import type { CodecMime, Kind } from "./types";
import { sourceKind } from "./types";

const OPUS_BITRATE_PER_CHANNEL = 32_000;
const OPUS_FRAME_DURATION = Time.Milli(20);
const AAC_BITRATE_PER_CHANNEL = 64_000;
const AAC_FRAME_SAMPLES = 1024; // AAC-LC encodes a fixed 1024 samples per frame.

// The WebCodecs/MP4 codec string for AAC-LC. "aac" is our user-facing shorthand.
const AAC_CODEC = "mp4a.40.2";

import { Framer } from "./framer";

// Selects the audio codec and its encoder settings. Either the bare codec name (all defaults) or an
// object with the mime plus tuning knobs.
export type Codec = Opus | Aac;

export type Opus = "opus" | OpusConfig;
export type Aac = "aac" | AacConfig;

// AAC encoder settings. AAC-LC has a fixed 1024-sample frame and no real-time tuning knobs, so
// bitrate is the only thing to configure.
export type AacConfig = {
	mime: "aac";

	bitrate?: number; // bits/sec, defaults to channelCount * 64kbps
};

// Opus encoder settings. bitrate and frameDuration also shape the catalog (decoders need them); the
// rest are encode-only knobs that map directly to the matching OpusEncoderConfig fields:
// https://developer.mozilla.org/en-US/docs/Web/API/AudioEncoder/configure#opus
export type OpusConfig = {
	mime: "opus";

	bitrate?: number; // bits/sec, defaults to channelCount * 32kbps
	// The type carries the unit (ms): build with Time.Milli(20). Opus supports 2.5-60ms, defaults to 20ms.
	frameDuration?: Time.Milli;
	complexity?: number; // 0-10, higher is better quality but more CPU
	packetlossperc?: number; // 0-100, expected loss the encoder optimizes for
	useinbandfec?: boolean; // in-band forward error correction
	usedtx?: boolean; // discontinuous transmission (silence suppression)
};

/** Cumulative encoder output totals, measured from the chunks the encoder produces. */
export interface Stats {
	/** Total frames encoded while serving. Monotonic; diff over an interval for a frame rate. */
	frames: number;

	/** Total bytes encoded while serving. Monotonic; diff over an interval for an upload bitrate. */
	bytes: number;
}

// Signals the encoder reads.
export type EncoderInput = {
	// Whether to publish (and encode) this rendition. When false the rendition drops out of the
	// catalog and stops encoding, but stays registered so a subscriber still gets an idle track.
	enabled: Getter<boolean>;

	// The broadcast to register the rendition on. Undefined resolves the config but has nowhere to publish.
	broadcast: Getter<Broadcast | undefined>;

	// The capture supplying PCM. Shared: one capture feeds any number of renditions, so build it
	// yourself and pass the same instance to each.
	capture: Getter<Capture | undefined>;
};

/** Constructor options: the wired inputs plus the live-editable tuning knobs. */
export type EncoderProps = Inputs<EncoderInput> & {
	// User tuning knobs. Seed a value or wire a Signal; also live-editable via the matching field.
	muted?: boolean | Signal<boolean>;
	volume?: number | Signal<number>;

	// Codec selection plus encoder settings. Defaults to "opus".
	codec?: Codec | Signal<Codec>;
};

type EncoderOutput = {
	// The catalog config published for this rendition, or undefined while there's no capture.
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

	readonly in: Readonlys<EncoderInput>;

	/** Silence the encoded audio without tearing down the capture graph. */
	muted: Signal<boolean>;
	/** Linear gain applied before encoding, where 1 is unity. */
	volume: Signal<number>;
	/** The live-editable codec selection plus its encoder settings. */
	codec: Signal<Codec>;

	readonly #out: EncoderOutput = {
		catalog: new Signal<Catalog.AudioConfig | undefined>(undefined),
		root: new Signal<AudioNode | undefined>(undefined),
		active: new Signal<boolean>(false),
		stats: new Signal<Stats>({ frames: 0, bytes: 0 }),
	};
	readonly out = readonlys(this.#out);

	// The encode chain currently publishing, or undefined while nothing is. The read loop pushes
	// into this; frames that arrive while it's undefined are dropped, which the framer treats as a
	// discontinuity and re-anchors on.
	#pipeline: Pipeline | undefined;

	#signals = new Effect();

	constructor(name: string, props?: EncoderProps) {
		this.name = name;
		this.in = {
			enabled: getter(props?.enabled ?? false),
			broadcast: getter(props?.broadcast),
			capture: getter(props?.capture),
		};
		this.muted = Signal.from(props?.muted ?? false);
		this.volume = Signal.from(props?.volume ?? 1);
		this.codec = Signal.from<Codec>(props?.codec ?? "opus");

		// Only the capture graph has a node to expose.
		this.#signals.run((effect) => {
			const capture = effect.get(this.in.capture);
			if (!capture) return;
			effect.proxy(this.#out.root, capture.out.root);
		});

		this.#signals.run(this.#runCapture.bind(this));
		this.#signals.run(this.#runConfig.bind(this));
		this.#signals.run(this.#runRegister.bind(this));
	}

	// Pump PCM off the capture into whatever is currently publishing, applying the volume knobs on
	// the way through. Tied to the capture's lifetime rather than the encoder's, so reconfiguring
	// (or muting) never has to reacquire the stream, which for a decoded file would be fatal.
	#runCapture(effect: Effect): void {
		const capture = effect.get(this.in.capture);
		if (!capture) return;

		const fanout = effect.get(capture.out.frames);
		if (!fanout) return;

		// Our own stream off the shared capture, so another rendition reading slowly can't take
		// frames from this one.
		const reader = fanout.subscribe(effect).getReader();
		effect.cleanup(() => void reader.cancel());

		const gain = new Gain();

		effect.spawn(async () => {
			for (;;) {
				const next = await Promise.race([reader.read(), effect.cancel]);
				if (!next?.value) break;

				const format = capture.out.format.peek();
				if (!format) continue;

				// Every rendition shares the captured frame, so gain returns a copy rather than
				// scaling in place; muting one rendition must not silence the rest.
				const frame = gain.apply(next.value, format.sampleRate);
				gain.set(this.muted.peek() ? 0 : this.volume.peek());

				// The config rebuilds when the channel count moves, so skip anything that arrives
				// mid-swap rather than framing it wrong.
				const pipeline = this.#pipeline;
				if (pipeline && pipeline.channelCount === frame.channels.length) pipeline.push(frame);
			}
		});
	}

	// Register the rendition on the broadcast, publish its config, and encode only while a subscriber
	// is attached (the demand gate). Re-registers cleanly when the broadcast swaps.
	#runRegister(effect: Effect): void {
		const broadcast = effect.get(this.in.broadcast);
		if (!broadcast) return;

		const rendition = broadcast.audio(this.name);
		effect.cleanup(() => rendition.close());

		// Publish the resolved config; undefined (no capture) drops it from the catalog.
		effect.proxy(rendition.config, this.out.catalog);

		effect.run((effect) => {
			const enabled = effect.get(this.in.enabled);
			const capture = effect.get(this.in.capture);
			const format = capture ? effect.get(capture.out.format) : undefined;
			const track = effect.get(rendition.track);
			effect.set(this.#out.active, enabled && !!format && !!track, false);
			if (!enabled || !format || !track) return;

			this.#encode(track, format, effect);
		});
	}

	#createConfig(captured: Format, codec: OpusConfig | AacConfig): Catalog.AudioConfig {
		// The catalog has to describe what the encoder emits, not what we feed it. A capture graph
		// already runs at a rate the codec supports, since Capture picks the AudioContext rate.
		// Decoded samples arrive at whatever rate the file was authored at, which Opus may not be
		// able to carry, so snap to one it can and let #encode resample into it.
		const rate = pickSampleRate(codec.mime, captured.sampleRate) ?? captured.sampleRate;

		const sampleRate = Catalog.u53(rate);
		const numberOfChannels = Catalog.u53(captured.channelCount);

		if (codec.mime === "aac") {
			return {
				codec: AAC_CODEC,
				sampleRate,
				numberOfChannels,
				bitrate: Catalog.u53(codec.bitrate ?? captured.channelCount * AAC_BITRATE_PER_CHANNEL),
				container: { kind: "legacy" } as const,
				// Frames are raw (no ADTS header), so the decoder needs the AudioSpecificConfig to init.
				description: Util.Hex.fromBytes(Util.Aac.audioSpecificConfig(rate, captured.channelCount)),
				// Each AAC-LC frame is 1024 samples; report that duration as the jitter hint.
				jitter: Catalog.u53(Math.ceil((AAC_FRAME_SAMPLES / rate) * 1000)),
			};
		}

		return {
			codec: "opus",
			sampleRate,
			numberOfChannels,
			bitrate: Catalog.u53(codec.bitrate ?? captured.channelCount * OPUS_BITRATE_PER_CHANNEL),
			container: { kind: "legacy" } as const,
			// jitter doubles as the Opus frame duration; toEncoderConfig converts it to µs for WebCodecs.
			jitter: Catalog.u53(codec.frameDuration ?? OPUS_FRAME_DURATION),
		};
	}

	// Derive the catalog from the captured format and the codec. Re-runs whenever either changes, so a
	// codec update (bitrate, frame duration) reconfigures without waiting for a channel-count change.
	//
	// Gated on `enabled` the same way the video encoder is: a disabled rendition has to drop out of
	// the catalog, and a sample source keeps its format while muted rather than tearing down.
	#runConfig(effect: Effect): void {
		const capture = effect.get(this.in.capture);
		const captured = capture ? effect.get(capture.out.format) : undefined;
		if (!effect.get(this.in.enabled) || !captured) {
			effect.set(this.#out.catalog, undefined);
			return;
		}

		const codec = normalizeCodec(effect.get(this.codec));
		effect.set(this.#out.catalog, this.#createConfig(captured, codec));
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
		if (codec.usedtx !== undefined) opus.usedtx = codec.usedtx;

		return opus;
	}

	// Encode captured audio frames into the track producer. The broadcast owns the track's lifetime, so
	// this only aborts it on a fatal encoder error, never on teardown.
	#encode(track: Moq.Track.Producer, format: Format, effect: Effect): void {
		effect.spawn(async () => {
			// We're using an async polyfill temporarily for Safari support.
			await Util.Libav.polyfill();

			effect.run((effect: Effect) => {
				const config = effect.get(this.out.catalog);
				if (!config) return;

				const capture = effect.get(this.in.capture);
				const source = capture ? effect.get(capture.in.source) : undefined;
				const kind: Kind = source ? sourceKind(source) : "auto";
				const encoderConfig = toEncoderConfig(config, kind, this.#opusOptions(effect));

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

				const framer = createFramer(config, config.sampleRate);

				const encoder = new AudioEncoder({
					output: (frame) => {
						if (frame.type !== "key") {
							throw new Error("only key frames are supported");
						}

						this.#out.stats.update((stats) => ({
							frames: stats.frames + 1,
							bytes: stats.bytes + frame.byteLength,
						}));

						// Each audio frame is its own group so the relay can forward it without
						// waiting for a group boundary. Loss is handled by the codec's PLC.
						track.writeFrame({
							payload: Container.Legacy.encodeFrame(frame, frame.timestamp as Time.Micro),
							timestamp: Time.Timestamp.fromMicros(frame.timestamp as Time.Micro),
						});
					},
					error: (err) => {
						console.error("encoder error", err);
						track.close(err);
					},
				});
				// A fatal error already closed the codec, and closing it twice throws.
				effect.cleanup(() => {
					if (encoder.state !== "closed") encoder.close();
				});

				console.debug("encoding audio", encoderConfig);
				encoder.configure(encoderConfig);

				const pipeline: Pipeline = {
					channelCount: config.numberOfChannels,
					push: (captured: AudioFrame) => {
						const input = resampler ? resampler.push(captured) : captured;
						if (!input) return;

						for (const data of framer.push(input)) {
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
								timestamp: data.timestamp,
								data: joined,
								transfer: [joined.buffer],
							});

							encoder.encode(frame);
							frame.close();
						}
					},
				};

				// Publish it last: the read loop starts pushing the moment this is visible.
				this.#pipeline = pipeline;
				effect.cleanup(() => {
					if (this.#pipeline === pipeline) this.#pipeline = undefined;
				});
			});
		});
	}

	close() {
		this.#signals.close();
	}
}

// Build the framer for a config, given the rate the PCM actually arrives at. That's the catalog rate
// for a capture graph, but a decoded file can arrive at a rate the codec doesn't carry (44100 for
// Opus), and the framer has to count the samples we're handed rather than the ones the encoder emits.
function createFramer(config: Catalog.AudioConfig, sampleRate: number): Framer {
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
	const duration = Time.Micro.fromMilli(Time.Milli(config.jitter ?? OPUS_FRAME_DURATION));
	return new Framer({
		sampleRate,
		channels: config.numberOfChannels,
		size: { duration },
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
// OpusConfig (carried in opusOptions) overrides them, so a caller can always opt out. DTX (silence
// suppression) is enabled for voice, where speech has natural gaps that collapse to tiny
// comfort-noise packets. Music has no useful silence to suppress, and "auto" leaves every knob to
// the browser.
function opusKindDefaults(kind: Kind): OpusEncoderConfigExt {
	switch (kind) {
		case "voice":
			return { application: "voip", signal: "voice", usedtx: true };
		case "music":
			return { application: "audio", signal: "music" };
		default:
			return {};
	}
}

// Build the WebCodecs encoder config from the catalog (decoder) config, a Kind hint, and any
// Opus-only knobs. Those knobs are kept out of the catalog since they only affect encoding. AAC has
// no such knobs, so it just uses the shared base fields (codec/sampleRate/channels/bitrate).
function toEncoderConfig(
	config: Catalog.AudioConfig,
	kind: Kind,
	opusOptions: OpusEncoderConfigExt,
): AudioEncoderConfig {
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

		// jitter carries the frame duration in ms; WebCodecs wants µs.
		if (config.jitter !== undefined) {
			opus.frameDuration = Time.Micro.fromMilli(Time.Milli(config.jitter));
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

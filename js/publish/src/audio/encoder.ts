import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Util from "@moq/hang/util";
import type * as Moq from "@moq/net";
import { Time } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";
import type * as Capture from "./capture";
import { type Pcm, SamplePcm, WorkletPcm } from "./pcm";
import { Resampler } from "./resampler";
import { isSampleSource, type Kind, normalizeSource, type SampleSource, type Source, sourceKind } from "./types";

const OPUS_BITRATE_PER_CHANNEL = 32_000;
const OPUS_FRAME_DURATION = Time.Milli(20);
const AAC_BITRATE_PER_CHANNEL = 64_000;
const AAC_FRAME_SAMPLES = 1024; // AAC-LC encodes a fixed 1024 samples per frame.

// The WebCodecs/MP4 codec string for AAC-LC. "aac" is our user-facing shorthand.
const AAC_CODEC = "mp4a.40.2";

// Compiled and inlined as a blob URL via vite-plugin-worklet.
import CaptureWorklet from "./capture-worklet.ts?worklet";
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

	// The microphone (or other) track supplying samples.
	source: Getter<Source | undefined>;
};

/** Constructor options: the wired inputs plus the live-editable tuning knobs. */
export type EncoderProps = Inputs<EncoderInput> & {
	// User tuning knobs. Seed a value or wire a Signal; also live-editable via the matching field.
	muted?: boolean | Signal<boolean>;
	volume?: number | Signal<number>;
	sampleRate?: number | Signal<number | undefined>;
	channelCount?: number | Signal<number | undefined>;

	// Codec selection plus encoder settings. Defaults to "opus".
	codec?: Codec | Signal<Codec>;
};

type EncoderOutput = {
	// The catalog config published for this rendition, or undefined while there's no capture.
	catalog: Signal<Catalog.AudioConfig | undefined>;
	// The tail of the capture graph, so callers can tap the (gain-adjusted) audio.
	root: Signal<AudioNode | undefined>;
	// True when a subscriber is attached and we're encoding.
	active: Signal<boolean>;
	// Cumulative output totals (frames, bytes) measured while serving.
	stats: Signal<Stats>;
};

// The PCM format we actually feed the encoder: the source's sample rate and the actual channel
// count (which can differ from the requested count on some platforms, e.g. Safari/macOS).
//
// This is not necessarily the rate we advertise. A decoded file arrives at whatever rate it was
// authored at, which Opus may not be able to carry, so #createConfig snaps the catalog rate.
type Captured = { sampleRate: number; channelCount: number };

// Which codec is in use, ignoring its tuning knobs.
type CodecMime = OpusConfig["mime"] | AacConfig["mime"];

/**
 * A single audio rendition encoder.
 *
 * Registers itself on the {@link Broadcast} under {@link name} (via `broadcast.audio(name)`), builds a
 * capture graph from the source track, and encodes samples only while a subscriber is attached (the
 * demand gate). Rename by constructing a new encoder; the name is not a signal.
 */
export class Encoder {
	/** The full track name of this rendition, e.g. `"audio/data"`. */
	readonly name: string;

	readonly in: Readonlys<EncoderInput>;

	/** Silence the encoded audio without tearing down the capture graph. */
	muted: Signal<boolean>;
	/** Linear gain applied before encoding, where 1 is unity. */
	volume: Signal<number>;
	/** Override the capture sample rate in Hz. Defaults to the track's own rate. */
	sampleRate: Signal<number | undefined>;
	/** Override the captured channel count. Defaults to the track's requested count. */
	channelCount: Signal<number | undefined>;
	/** The live-editable codec selection plus its encoder settings. */
	codec: Signal<Codec>;

	// Observed capture format. #out.catalog is derived from this plus the codec, so the
	// worklet handlers only ever write here, never read-modify-write the catalog.
	#captured = new Signal<Captured | undefined>(undefined);

	// Only the mime picks the capture sample rate, so the capture graph tracks this rather than the
	// whole codec signal. Otherwise changing an encode-only knob (bitrate, complexity) would rebuild
	// the AudioContext, drop #worklet, and close the track we're publishing.
	#codecMime: Signal<CodecMime>;

	readonly #out: EncoderOutput = {
		catalog: new Signal<Catalog.AudioConfig | undefined>(undefined),
		root: new Signal<AudioNode | undefined>(undefined),
		active: new Signal<boolean>(false),
		stats: new Signal<Stats>({ frames: 0, bytes: 0 }),
	};
	readonly out = readonlys(this.#out);

	// The live PCM source, whether that's a capture graph or a stream of decoded samples.
	#pcm = new Signal<Pcm | undefined>(undefined);

	#signals = new Effect();

	constructor(name: string, props?: EncoderProps) {
		this.name = name;
		this.in = {
			enabled: getter(props?.enabled ?? false),
			broadcast: getter(props?.broadcast),
			source: getter(props?.source),
		};
		this.muted = Signal.from(props?.muted ?? false);
		this.volume = Signal.from(props?.volume ?? 1);
		this.sampleRate = Signal.from<number | undefined>(props?.sampleRate);
		this.channelCount = Signal.from<number | undefined>(props?.channelCount);
		this.codec = Signal.from<Codec>(props?.codec ?? "opus");
		this.#codecMime = new Signal(normalizeCodec(this.codec.peek()).mime);

		this.#signals.run((effect) => {
			this.#codecMime.set(normalizeCodec(effect.get(this.codec)).mime);
		});

		this.#signals.run(this.#runSource.bind(this));
		this.#signals.run(this.#runGain.bind(this));
		this.#signals.run(this.#runConfig.bind(this));
		this.#signals.run(this.#runRegister.bind(this));
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
			const pcm = effect.get(this.#pcm);
			const track = effect.get(rendition.track);
			effect.set(this.#out.active, enabled && !!pcm && !!track, false);
			if (!enabled || !pcm || !track) return;

			this.#encode(track, pcm, effect);
		});
	}

	#runSource(effect: Effect): void {
		const rawSource = effect.get(this.in.source);
		if (!rawSource) return;

		// Samples we decoded ourselves skip Web Audio entirely: they already carry the container's
		// own timestamps, and routing them through a realtime graph would resample and restamp them.
		//
		// Note this runs even while disabled. There's no device to release, the stream is one-shot
		// (dropping it means we can never read it again), and it's paced against a wall clock, so
		// stalling it while muted would leave the audio behind the video for good on unmute.
		if (isSampleSource(rawSource)) {
			this.#runSamples(rawSource, effect);
			return;
		}

		// Releasing the capture device while muted is the whole point of gating on `enabled`.
		if (!effect.get(this.in.enabled)) return;

		const source = normalizeSource(rawSource);
		const settings = source.track.getSettings();
		const overrideSampleRate = effect.get(this.sampleRate);
		const mime = effect.get(this.#codecMime);
		const sampleRate = pickSampleRate(mime, overrideSampleRate ?? settings.sampleRate);

		if (overrideSampleRate !== undefined && sampleRate !== overrideSampleRate) {
			console.warn(`${mime} does not support ${overrideSampleRate}Hz, capturing at ${sampleRate}Hz`);
		}

		// macOS misreports a mono mic as stereo: getSettings().channelCount is undefined and
		// MediaStreamAudioSourceNode.channelCount defaults to 2, so the graph carries (and Opus
		// encodes) duplicated mono as stereo. Prefer an explicitly requested channel count, from
		// the prop or the track's applied getUserMedia constraint, and force the worklet to mix to it.
		const requestedChannels = effect.get(this.channelCount) ?? requestedChannelCount(source.track);

		const context = new AudioContext({
			latencyHint: "interactive",
			sampleRate,
		});
		effect.cleanup(() => context.close());

		const root = new MediaStreamAudioSourceNode(context, {
			mediaStream: new MediaStream([source.track]),
		});
		effect.cleanup(() => root.disconnect());

		const gain = new GainNode(context, {
			gain: this.volume.peek(),
		});
		root.connect(gain);
		effect.cleanup(() => gain.disconnect());

		// Async because we need to wait for the worklet to be registered.
		effect.spawn(async () => {
			// Race the module load against teardown. If teardown wins, `loaded` is undefined and we bail
			// before constructing the node: the module registration was abandoned, so building against its
			// name would throw. Gate on the race result, not `context.state`, because `AudioContext.close()`
			// only flips `.state` to "closed" synchronously on Chrome (Firefox/Safari report "suspended").
			const loaded = await Promise.race([
				context.audioWorklet.addModule(CaptureWorklet).then(() => true),
				effect.cancel,
			]);
			if (!loaded) return;

			const channelCount = requestedChannels ?? settings.channelCount ?? root.channelCount;
			const worklet = new AudioWorkletNode(context, "capture", {
				numberOfInputs: 1,
				numberOfOutputs: 0,
				channelCount,
				// "explicit" forces Web Audio to (down)mix the input to channelCount before the
				// worklet sees it. The default "max" just follows the input, which is the unreliable
				// path on macOS. Only force it when we actually have a requested count to honor.
				channelCountMode: requestedChannels !== undefined ? "explicit" : "max",
				// Stamp audio against the same wall clock as video (see video/processor.ts), so both
				// tracks share an epoch and stay in sync.
				processorOptions: { zero: performance.now() * 1000 },
			});

			const pcm = new WorkletPcm(worklet, gain);
			effect.cleanup(() => pcm.close());

			gain.connect(worklet);

			// The channel count is unreliable on some platforms (Apple's Safari), so record the
			// format the frames actually arrive in rather than the one we asked for.
			pcm.listen(effect, (frame) => {
				const channelCount = frame.channels.length;
				if (!channelCount) return;
				if (this.#captured.peek()?.channelCount === channelCount) return;

				this.#captured.set({ sampleRate: pcm.sampleRate, channelCount });
			});
			effect.cleanup(() => {
				this.#captured.set(undefined);
			});

			// Only publish the source once the graph is fully wired, since #runGain reacts to it.
			effect.set(this.#pcm, pcm);
			effect.set(this.#out.root, gain);
		});
	}

	// Samples arrive already decoded, so there's no capture graph to build and no format to
	// discover: the source declares what the decoder produced.
	#runSamples(source: SampleSource, effect: Effect): void {
		const { sampleRate, channelCount } = source;

		const pcm = new SamplePcm({ samples: source.samples, sampleRate, channelCount });
		effect.cleanup(() => pcm.close());

		effect.set(this.#captured, { sampleRate, channelCount }, undefined);
		effect.set(this.#pcm, pcm);
	}

	#createConfig(captured: Captured, codec: OpusConfig | AacConfig): Catalog.AudioConfig {
		// The catalog has to describe what the encoder emits, not what we feed it. A capture graph
		// already runs at a rate the codec supports, since #runSource picks the AudioContext rate.
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
		const captured = effect.get(this.#captured);
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

	#runGain(effect: Effect): void {
		const pcm = effect.get(this.#pcm);
		if (!pcm) return;

		pcm.gain(effect.get(this.muted) ? 0 : effect.get(this.volume));
	}

	// Encode captured audio frames into the track producer. The broadcast owns the track's lifetime, so
	// this only aborts it on a fatal encoder error, never on teardown.
	#encode(track: Moq.Track.Producer, pcm: Pcm, effect: Effect): void {
		effect.spawn(async () => {
			// We're using an async polyfill temporarily for Safari support.
			await Util.Libav.polyfill();

			effect.run((effect: Effect) => {
				const config = effect.get(this.out.catalog);
				if (!config) return;

				const source = effect.get(this.in.source);
				const kind: Kind = source ? sourceKind(source) : "auto";
				const encoderConfig = toEncoderConfig(config, kind, this.#opusOptions(effect));

				// WebCodecs rejects input whose rate doesn't match the encoder config outright, so
				// anything arriving at a rate the codec can't carry (a 44.1kHz file as Opus) has to
				// be converted first. A capture graph already runs at the right rate, so this is
				// usually nothing.
				const resampler =
					pcm.sampleRate === config.sampleRate
						? undefined
						: new Resampler({
								from: pcm.sampleRate,
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

				pcm.listen(effect, (captured: Capture.AudioFrame) => {
					// #runSource watches the channel count and rebuilds the config when it moves, so
					// just skip whatever arrives in the meantime rather than framing it wrong.
					if (captured.channels.length !== config.numberOfChannels) return;

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
				});
			});
		});
	}

	close() {
		this.#signals.close();
	}
}

// getConstraints() echoes the constraints applied via getUserMedia, which (unlike getSettings)
// survives the macOS mono->stereo misreport. Returns the requested channel count, if any.
function requestedChannelCount(track: MediaStreamTrack): number | undefined {
	const constraint = track.getConstraints().channelCount;
	if (constraint === undefined) return undefined;
	if (typeof constraint === "number") return constraint;
	return constraint.exact ?? constraint.ideal ?? constraint.max ?? constraint.min;
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

// Pick the rate to run the capture AudioContext at, given what the source reports (or the caller
// asked for). Codecs only encode at a discrete set of rates, so snap to one they actually support.
//
// This has to happen at the AudioContext rather than just in the catalog. A context running at
// 44100 hands 44100 AudioData to an encoder that resamples to 48000 behind our back, and we then
// advertise 44100: a rate no Opus decoder can honor, which Safari rejects outright. Snapping here
// keeps one rate across the capture graph, the encoder config, and the catalog.
function pickSampleRate(mime: CodecMime, requested: number | undefined): number | undefined {
	// Treat a nonsense rate as unknown. It would otherwise snap to the codec's floor (7350Hz for AAC)
	// instead of throwing NotSupportedError at the AudioContext where it's obvious.
	const rate = requested !== undefined && Number.isFinite(requested) && requested > 0 ? requested : undefined;

	if (mime === "opus") {
		// An unknown rate (captureStream reports none) would let the AudioContext fall back to the
		// machine's output rate, which is 44100 on most Macs. Ask for full-band Opus instead.
		return Util.Opus.pickRate(rate ?? Util.Opus.DEFAULT_SAMPLE_RATE);
	}

	// The AAC table includes 44100, so an unknown rate can safely fall through to the context default.
	return rate !== undefined ? Util.Aac.pickRate(rate) : undefined;
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

import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Util from "@moq/hang/util";
import type * as Moq from "@moq/net";
import { Time } from "@moq/net";
import { Effect, type Getter, Signal } from "@moq/signals";
import type * as Capture from "./capture";
import { type Kind, normalizeSource, type Source } from "./types";

const GAIN_MIN = 0.001;
const FADE_TIME = 0.2;
const OPUS_BITRATE_PER_CHANNEL = 32_000;
const OPUS_FRAME_DURATION = Time.Milli(20);
const AAC_BITRATE_PER_CHANNEL = 64_000;
const AAC_FRAME_SAMPLES = 1024; // AAC-LC encodes a fixed 1024 samples per frame.

// The WebCodecs/MP4 codec string for AAC-LC. "aac" is our user-facing shorthand.
const AAC_CODEC = "mp4a.40.2";

// Compiled and inlined as a blob URL via vite-plugin-worklet.
import CaptureWorklet from "./capture-worklet.ts?worklet";

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

// The initial values for our signals.
export type EncoderProps = {
	enabled?: boolean | Signal<boolean>;
	source?: Source | Signal<Source | undefined>;

	muted?: boolean | Signal<boolean>;
	volume?: number | Signal<number>;
	sampleRate?: number | Signal<number | undefined>;
	channelCount?: number | Signal<number | undefined>;

	// Codec selection plus encoder settings. Defaults to "opus".
	codec?: Codec | Signal<Codec>;

	container?: Catalog.Container;
};

// The audio format observed from the capture worklet: the AudioContext sample rate and the actual
// channel count (which can differ from the requested count on some platforms, e.g. Safari/macOS).
type Captured = { sampleRate: number; channelCount: number };

export class Encoder {
	static readonly TRACK = "audio/data";
	static readonly PRIORITY = Catalog.PRIORITY.audio;

	enabled: Signal<boolean>;

	muted: Signal<boolean>;
	volume: Signal<number>;
	sampleRate: Signal<number | undefined>;
	channelCount: Signal<number | undefined>;
	codec: Signal<Codec>;

	source: Signal<Source | undefined>;

	#catalog = new Signal<Catalog.Audio | undefined>(undefined);
	readonly catalog: Getter<Catalog.Audio | undefined> = this.#catalog;

	// Observed capture format. #config (and thus #catalog) is derived from this plus the codec, so the
	// worklet handlers only ever write here, never read-modify-write #config.
	#captured = new Signal<Captured | undefined>(undefined);

	#config = new Signal<Catalog.AudioConfig | undefined>(undefined);
	readonly config: Getter<Catalog.AudioConfig | undefined> = this.#config;

	#worklet = new Signal<AudioWorkletNode | undefined>(undefined);

	#gain = new Signal<GainNode | undefined>(undefined);
	readonly root: Getter<AudioNode | undefined> = this.#gain;

	active = new Signal<boolean>(false);

	#stats = new Signal<Stats>({ frames: 0, bytes: 0 });
	/** Cumulative encoder output totals (frames, bytes) measured while serving. */
	readonly stats: Getter<Stats> = this.#stats;

	#signals = new Effect();

	constructor(props?: EncoderProps) {
		this.source = Signal.from(props?.source);
		this.enabled = Signal.from(props?.enabled ?? false);
		this.muted = Signal.from(props?.muted ?? false);
		this.volume = Signal.from(props?.volume ?? 1);
		this.sampleRate = Signal.from<number | undefined>(props?.sampleRate);
		this.channelCount = Signal.from<number | undefined>(props?.channelCount);
		this.codec = Signal.from<Codec>(props?.codec ?? "opus");

		this.#signals.run(this.#runSource.bind(this));
		this.#signals.run(this.#runGain.bind(this));
		this.#signals.run(this.#runConfig.bind(this));
		this.#signals.run(this.#runCatalog.bind(this));
	}

	#runSource(effect: Effect): void {
		const values = effect.getAll([this.enabled, this.source]);
		if (!values) return;
		const [_, rawSource] = values;
		const source = normalizeSource(rawSource);

		const settings = source.track.getSettings();
		const overrideSampleRate = effect.get(this.sampleRate);
		const sampleRate = overrideSampleRate ?? settings.sampleRate;

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
			await context.audioWorklet.addModule(CaptureWorklet);
			if (context.state === "closed") return;

			const channelCount = requestedChannels ?? settings.channelCount ?? root.channelCount;
			const worklet = new AudioWorkletNode(context, "capture", {
				numberOfInputs: 1,
				numberOfOutputs: 0,
				channelCount,
				// "explicit" forces Web Audio to (down)mix the input to channelCount before the
				// worklet sees it. The default "max" just follows the input, which is the unreliable
				// path on macOS. Only force it when we actually have a requested count to honor.
				channelCountMode: requestedChannels !== undefined ? "explicit" : "max",
				// Stamp audio against the same wall clock as video (see video/polyfill.ts), so both
				// tracks share an epoch and stay in sync.
				processorOptions: { zero: performance.now() * 1000 },
			});

			effect.set(this.#worklet, worklet);

			// The information about channels count can be unreliable on different platforms (Apple's Safari).
			// Try to get the first audio frame and only then record the captured format.
			effect.event(
				worklet.port,
				"message",
				(event: Event) => {
					const data = (event as MessageEvent<Capture.AudioFrame>).data;
					const channelCount = data.channels.length;
					if (!channelCount) return;

					this.#captured.set({ sampleRate: worklet.context.sampleRate, channelCount });
				},
				{ once: true },
			);
			worklet.port.start();
			effect.cleanup(() => {
				this.#captured.set(undefined);
			});

			gain.connect(worklet);
			effect.cleanup(() => worklet.disconnect());

			// Only set the gain after the worklet is registered.
			effect.set(this.#gain, gain);
		});
	}

	#createConfig(captured: Captured, codec: OpusConfig | AacConfig): Catalog.AudioConfig {
		const sampleRate = Catalog.u53(captured.sampleRate);
		const numberOfChannels = Catalog.u53(captured.channelCount);

		if (codec.mime === "aac") {
			return {
				codec: AAC_CODEC,
				sampleRate,
				numberOfChannels,
				bitrate: Catalog.u53(codec.bitrate ?? captured.channelCount * AAC_BITRATE_PER_CHANNEL),
				container: { kind: "legacy" } as const,
				// Frames are raw (no ADTS header), so the decoder needs the AudioSpecificConfig to init.
				description: Util.Hex.fromBytes(
					Util.Aac.audioSpecificConfig(captured.sampleRate, captured.channelCount),
				),
				// Each AAC-LC frame is 1024 samples; report that duration as the jitter hint.
				jitter: Catalog.u53(Math.ceil((AAC_FRAME_SAMPLES / captured.sampleRate) * 1000)),
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

	// Derive #config from the captured format and the codec. Re-runs whenever either changes, so a
	// codec update (bitrate, frame duration) reconfigures without waiting for a channel-count change.
	#runConfig(effect: Effect): void {
		const captured = effect.get(this.#captured);
		if (!captured) {
			effect.set(this.#config, undefined);
			return;
		}

		const codec = normalizeCodec(effect.get(this.codec));
		effect.set(this.#config, this.#createConfig(captured, codec));
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
		const gain = effect.get(this.#gain);
		if (!gain) return;

		effect.cleanup(() => gain.gain.cancelScheduledValues(gain.context.currentTime));

		const volume = effect.get(this.muted) ? 0 : effect.get(this.volume);
		if (volume < GAIN_MIN) {
			gain.gain.exponentialRampToValueAtTime(GAIN_MIN, gain.context.currentTime + FADE_TIME);
			gain.gain.setValueAtTime(0, gain.context.currentTime + FADE_TIME + 0.01);
		} else {
			gain.gain.exponentialRampToValueAtTime(volume, gain.context.currentTime + FADE_TIME);
		}
	}

	serve(track: Moq.Track.Producer, effect: Effect): void {
		const values = effect.getAll([this.enabled, this.#worklet]);
		if (!values) return;
		const [_, worklet] = values;

		effect.set(this.active, true, false);

		effect.cleanup(() => track.close());

		effect.spawn(async () => {
			// We're using an async polyfill temporarily for Safari support.
			await Util.Libav.polyfill();

			const encoder = new AudioEncoder({
				output: (frame) => {
					if (frame.type !== "key") {
						throw new Error("only key frames are supported");
					}

					this.#stats.update((stats) => ({
						frames: stats.frames + 1,
						bytes: stats.bytes + frame.byteLength,
					}));

					// Each audio frame is its own group so the relay can forward it without
					// waiting for a group boundary. Loss is handled by the codec's PLC.
					track.writeFrame({
						data: Container.Legacy.encodeFrame(frame, frame.timestamp as Time.Micro),
						timestamp: Time.Timestamp.fromMicros(frame.timestamp as Time.Micro),
					});
				},
				error: (err) => {
					console.error("encoder error", err);
					track.close(err);
				},
			});
			effect.cleanup(() => encoder.close());

			let config: Catalog.AudioConfig | undefined;
			effect.run((effect: Effect) => {
				config = effect.get(this.#config);
				if (!config) return;

				const source = effect.get(this.source);
				const kind: Kind = source ? normalizeSource(source).kind : "auto";
				const encoderConfig = toEncoderConfig(config, kind, this.#opusOptions(effect));

				console.debug("encoding audio", encoderConfig);
				encoder.configure(encoderConfig);
			});

			effect.event(worklet.port, "message", (event: Event) => {
				const data = (event as MessageEvent<Capture.AudioFrame>).data;
				const channelCount = data.channels.length;
				if (!channelCount) return;

				if (!config || channelCount !== config.numberOfChannels) {
					this.#captured.set({ sampleRate: worklet.context.sampleRate, channelCount });
					return;
				}

				const channels = data.channels;
				const joinedLength = channels.reduce((a, b) => a + b.length, 0);
				const joined = new Float32Array(joinedLength);

				channels.reduce((offset: number, channel: Float32Array): number => {
					joined.set(channel, offset);
					return offset + channel.length;
				}, 0);

				const frame = new AudioData({
					format: "f32-planar",
					sampleRate: worklet.context.sampleRate,
					numberOfFrames: channels[0].length,
					numberOfChannels: channels.length,
					timestamp: data.timestamp,
					data: joined,
					transfer: [joined.buffer],
				});

				encoder.encode(frame);
				frame.close();
			});
			worklet.port.start();
		});
	}

	#runCatalog(effect: Effect): void {
		const config = effect.get(this.#config);
		if (!config) {
			effect.set(this.#catalog, undefined);
			return;
		}

		const catalog: Catalog.Audio = {
			renditions: { [Encoder.TRACK]: config },
		};

		effect.set(this.#catalog, catalog);
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

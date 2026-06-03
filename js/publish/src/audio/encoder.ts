import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Util from "@moq/hang/util";
import type * as Moq from "@moq/net";
import type { Time } from "@moq/net";
import { Effect, type Getter, Signal } from "@moq/signals";
import type * as Capture from "./capture";
import { type Kind, normalizeSource, type Source } from "./types";

const GAIN_MIN = 0.001;
const FADE_TIME = 0.2;
const OPUS_BITRATE_PER_CHANNEL = 32_000;
const OPUS_FRAME_DURATION = 20;

// Compiled and inlined as a blob URL via vite-plugin-worklet.
import CaptureWorklet from "./capture-worklet.ts?worklet";

// The initial values for our signals.
export type EncoderProps = {
	enabled?: boolean | Signal<boolean>;
	source?: Source | Signal<Source | undefined>;

	muted?: boolean | Signal<boolean>;
	volume?: number | Signal<number>;
	sampleRate?: number | Signal<number | undefined>;
	channelCount?: number | Signal<number | undefined>;

	container?: Catalog.Container;
};

export class Encoder {
	static readonly TRACK = "audio/data";
	static readonly PRIORITY = Catalog.PRIORITY.audio;

	enabled: Signal<boolean>;

	muted: Signal<boolean>;
	volume: Signal<number>;
	sampleRate: Signal<number | undefined>;
	channelCount: Signal<number | undefined>;

	source: Signal<Source | undefined>;

	#catalog = new Signal<Catalog.Audio | undefined>(undefined);
	readonly catalog: Getter<Catalog.Audio | undefined> = this.#catalog;

	#config = new Signal<Catalog.AudioConfig | undefined>(undefined);
	readonly config: Getter<Catalog.AudioConfig | undefined> = this.#config;

	#worklet = new Signal<AudioWorkletNode | undefined>(undefined);

	#gain = new Signal<GainNode | undefined>(undefined);
	readonly root: Getter<AudioNode | undefined> = this.#gain;

	active = new Signal<boolean>(false);

	#signals = new Effect();

	constructor(props?: EncoderProps) {
		this.source = Signal.from(props?.source);
		this.enabled = Signal.from(props?.enabled ?? false);
		this.muted = Signal.from(props?.muted ?? false);
		this.volume = Signal.from(props?.volume ?? 1);
		this.sampleRate = Signal.from<number | undefined>(props?.sampleRate);
		this.channelCount = Signal.from<number | undefined>(props?.channelCount);

		this.#signals.run(this.#runSource.bind(this));
		this.#signals.run(this.#runGain.bind(this));
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
			});

			effect.set(this.#worklet, worklet);

			// The information about channels count can be unreliable on different platforms (Apple's Safari).
			// Try to get the first audio frame and only then create the configuration.
			effect.event(
				worklet.port,
				"message",
				(event: Event) => {
					const data = (event as MessageEvent<Capture.AudioFrame>).data;
					const channelCount = data.channels.length;
					if (!channelCount) return;

					this.#config.set(this.#createConfig(worklet, channelCount));
				},
				{ once: true },
			);
			worklet.port.start();
			effect.cleanup(() => {
				this.#config.set(undefined);
			});

			gain.connect(worklet);
			effect.cleanup(() => worklet.disconnect());

			// Only set the gain after the worklet is registered.
			effect.set(this.#gain, gain);
		});
	}

	#createConfig(worklet: AudioWorkletNode, channelCount: number): Catalog.AudioConfig {
		return {
			codec: "opus",
			sampleRate: Catalog.u53(worklet.context.sampleRate),
			numberOfChannels: Catalog.u53(channelCount),
			bitrate: Catalog.u53(channelCount * OPUS_BITRATE_PER_CHANNEL),
			container: { kind: "legacy" } as const,
			// TODO parse the actual frame duration instead of assuming 20ms.
			// Opus supports 2.5–60ms but 20ms is the real-time default.
			jitter: Catalog.u53(OPUS_FRAME_DURATION),
		};
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

	serve(track: Moq.Track, effect: Effect): void {
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

					// Each audio frame is its own group so the relay can forward it without
					// waiting for a group boundary. Loss is handled by the codec's PLC.
					track.writeFrame(Container.Legacy.encodeFrame(frame, frame.timestamp as Time.Micro));
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
				const encoderConfig = toEncoderConfig(config, kind);

				console.debug("encoding audio", encoderConfig);
				encoder.configure(encoderConfig);
			});

			effect.event(worklet.port, "message", (event: Event) => {
				const data = (event as MessageEvent<Capture.AudioFrame>).data;
				const channelCount = data.channels.length;
				if (!channelCount) return;

				if (!config || channelCount !== config.numberOfChannels) {
					this.#config.set(this.#createConfig(worklet, channelCount));
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

// `application` and `signal` are in the WebCodecs spec but missing from lib.dom.d.ts.
// https://www.w3.org/TR/webcodecs-opus-codec-registration/#dom-opusencoderconfig
interface OpusEncoderConfigExt extends OpusEncoderConfig {
	application?: "voip" | "audio" | "lowdelay";
	signal?: "auto" | "voice" | "music";
}

// Build the WebCodecs encoder config from the catalog (decoder) config plus a Kind hint.
// Opus-only knobs are kept out of the catalog since they only affect encoding.
function toEncoderConfig(config: Catalog.AudioConfig, kind: Kind): AudioEncoderConfig {
	const encoderConfig: AudioEncoderConfig = {
		codec: config.codec,
		sampleRate: config.sampleRate,
		numberOfChannels: config.numberOfChannels,
		bitrate: config.bitrate,
	};

	if (config.codec === "opus" && kind !== "auto") {
		const opus: OpusEncoderConfigExt = {
			application: kind === "voice" ? "voip" : "audio",
			signal: kind,
		};
		encoderConfig.opus = opus;
	}

	return encoderConfig;
}

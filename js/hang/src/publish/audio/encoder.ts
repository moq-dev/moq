import type * as Moq from "@moq/lite";
import { Time } from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import * as Catalog from "../../catalog";
import { u53 } from "../../catalog/integers";
import * as Container from "../../container";
import * as libav from "../../util/libav";
import type * as Capture from "./capture";
import type { Source } from "./types";

const GAIN_MIN = 0.001;
const FADE_TIME = 0.2;

// Unfortunately, we need to use a Vite-exclusive import for now.
import CaptureWorklet from "./capture-worklet.ts?worker&url";

// The initial values for our signals.
export type EncoderProps = {
	enabled?: boolean | Signal<boolean>;
	source?: Source | Signal<Source | undefined>;

	muted?: boolean | Signal<boolean>;
	volume?: number | Signal<number>;

	// The size of each group. Larger groups mean fewer drops but the viewer can fall further behind.
	// NOTE: Each frame is always flushed to the network immediately.
	maxLatency?: Time.Milli;

	container?: Catalog.Container;
};

export class Encoder {
	static readonly TRACK = "audio/data";
	static readonly PRIORITY = Catalog.PRIORITY.audio;

	enabled: Signal<boolean>;

	muted: Signal<boolean>;
	volume: Signal<number>;
	maxLatency: Time.Milli;
	#container: Catalog.Container;

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
		this.maxLatency = props?.maxLatency ?? (100 as Time.Milli); // Default is a group every 100ms
		this.#container = props?.container ?? { kind: "legacy" };

		this.#signals.effect(this.#runSource.bind(this));
		this.#signals.effect(this.#runConfig.bind(this));
		this.#signals.effect(this.#runGain.bind(this));
		this.#signals.effect(this.#runCatalog.bind(this));
	}

	#runSource(effect: Effect): void {
		const enabled = effect.get(this.enabled);
		if (!enabled) return;

		const source = effect.get(this.source);
		if (!source) return;

		const settings = source.getSettings();

		const context = new AudioContext({
			latencyHint: "interactive",
			sampleRate: settings.sampleRate,
		});
		effect.cleanup(() => context.close());

		const root = new MediaStreamAudioSourceNode(context, {
			mediaStream: new MediaStream([source]),
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

			const worklet = new AudioWorkletNode(context, "capture", {
				numberOfInputs: 1,
				numberOfOutputs: 0,
				channelCount: settings.channelCount ?? root.channelCount,
			});

			effect.set(this.#worklet, worklet);

			gain.connect(worklet);
			effect.cleanup(() => worklet.disconnect());

			// Only set the gain after the worklet is registered.
			effect.set(this.#gain, gain);
		});
	}

	#runConfig(effect: Effect): void {
		const source = effect.get(this.source);
		if (!source) return;

		const worklet = effect.get(this.#worklet);
		if (!worklet) return;

		const config = {
			codec: "opus",
			sampleRate: u53(worklet.context.sampleRate),
			numberOfChannels: u53(worklet.channelCount),
			bitrate: u53(worklet.channelCount * 32_000),
			container: this.#container,
		};

		effect.set(this.#config, config);
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
		const enabled = effect.get(this.enabled);
		if (!enabled) return;

		const values = effect.getAll([this.#worklet, this.#config]);
		if (!values) return;
		const [worklet, config] = values;

		effect.set(this.active, true, false);

		const producer = new Container.Legacy.Producer(track);
		effect.cleanup(() => producer.close());

		let lastKeyframe: Time.Micro | undefined;

		effect.spawn(async () => {
			// We're using an async polyfill temporarily for Safari support.
			await libav.polyfill();

			const encoder = new AudioEncoder({
				output: (frame) => {
					if (frame.type !== "key") {
						throw new Error("only key frames are supported");
					}

					let keyframe = false;
					if (!lastKeyframe || lastKeyframe + Time.Micro.fromMilli(this.maxLatency) <= frame.timestamp) {
						lastKeyframe = frame.timestamp as Time.Micro;
						keyframe = true;
					}

					producer.encode(frame, frame.timestamp as Time.Micro, keyframe);
				},
				error: (err) => {
					console.error("encoder error", err);
					producer.close(err);
					worklet.port.onmessage = null;
				},
			});
			effect.cleanup(() => encoder.close());

			console.debug("encoding audio", config);
			encoder.configure(config);

			worklet.port.onmessage = ({ data }: { data: Capture.AudioFrame }) => {
				const channels = data.channels.slice(0, worklet.channelCount);
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
			};
			effect.cleanup(() => {
				worklet.port.onmessage = null;
			});
		});
	}

	#runCatalog(effect: Effect): void {
		const config = effect.get(this.#config);
		if (!config) return;

		const catalog: Catalog.Audio = {
			renditions: { [Encoder.TRACK]: config },
			priority: Encoder.PRIORITY,
		};

		effect.set(this.#catalog, catalog);
	}

	close() {
		this.#signals.close();
	}
}

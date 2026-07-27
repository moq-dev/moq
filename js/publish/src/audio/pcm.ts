import type { Time } from "@moq/net";
import type { Effect } from "@moq/signals";
import type { AudioFrame } from "./capture";

// Ramp gain over this long so a mute or volume change doesn't click.
const FADE = 0.2; // seconds

// Web Audio can't ramp exponentially to zero, so approach silence and then cut.
const GAIN_MIN = 0.001;

/**
 * A live source of PCM frames feeding the encoder.
 *
 * Two shapes end up here: the Web Audio capture graph built around a MediaStreamTrack, and samples
 * decoded somewhere else and handed over as a stream. Both push frames at whoever is listening, so
 * a frame that arrives while the encoder is reconfiguring is dropped rather than queued.
 */
export interface Pcm {
	/** The sample rate every frame is delivered at, in Hz. */
	readonly sampleRate: number;

	/**
	 * The channel count, when the source declares one up front. Undefined when it can only be
	 * observed from the first frame, which is the case for a capture graph.
	 */
	readonly channelCount?: number;

	/** The tail of the Web Audio graph, when there is one, so callers can tap the audio. */
	readonly root?: AudioNode;

	/** Set the linear gain applied before encoding, where 1 is unity. Ramped so it doesn't click. */
	gain(value: number): void;

	/** Deliver each frame to `fn` until `effect` is torn down. */
	listen(effect: Effect, fn: (frame: AudioFrame) => void): void;

	/** Stop delivering frames and release the source. */
	close(): void;
}

/** PCM off the Web Audio capture graph, sampled from a MediaStreamTrack by an AudioWorklet. */
export class WorkletPcm implements Pcm {
	readonly sampleRate: number;

	/** The gain node the worklet is connected to, which is also where the volume ramps land. */
	readonly root: GainNode;

	readonly #node: AudioWorkletNode;

	/** Wrap a running capture worklet and the gain node feeding it. */
	constructor(node: AudioWorkletNode, root: GainNode) {
		this.#node = node;
		this.root = root;
		this.sampleRate = node.context.sampleRate;
	}

	gain(value: number): void {
		const param = this.root.gain;
		const now = this.root.context.currentTime;

		param.cancelScheduledValues(now);

		if (value < GAIN_MIN) {
			param.exponentialRampToValueAtTime(GAIN_MIN, now + FADE);
			param.setValueAtTime(0, now + FADE + 0.01);
		} else {
			param.exponentialRampToValueAtTime(value, now + FADE);
		}
	}

	listen(effect: Effect, fn: (frame: AudioFrame) => void): void {
		effect.event(this.#node.port, "message", (event: Event) => {
			fn((event as MessageEvent<AudioFrame>).data);
		});
		this.#node.port.start();
	}

	close(): void {
		this.#node.disconnect();
	}
}

/** Constructor options for {@link SamplePcm}: the samples plus the format they're already in. */
export interface SampleProps {
	/** The decoded samples, in presentation order. */
	samples: ReadableStream<AudioData>;

	/** The sample rate the samples were decoded at, in Hz. */
	sampleRate: number;

	/** The channel count the samples were decoded at. */
	channelCount: number;
}

/**
 * PCM decoded elsewhere and handed over as a stream, e.g. a demuxed file.
 *
 * Reading starts immediately rather than waiting for a listener, so the source keeps advancing on
 * its own clock and stays in sync with the matching video track.
 */
export class SamplePcm implements Pcm {
	readonly sampleRate: number;
	readonly channelCount: number;

	readonly #reader: ReadableStreamDefaultReader<AudioData>;
	readonly #listeners = new Set<(frame: AudioFrame) => void>();

	// Gain is applied per sample, walking toward the target rather than jumping to it.
	#gain = 1;
	#target = 1;

	constructor(props: SampleProps) {
		this.sampleRate = props.sampleRate;
		this.channelCount = props.channelCount;
		this.#reader = props.samples.getReader();

		void this.#pump().catch((err) => console.error("audio sample stream failed:", err));
	}

	gain(value: number): void {
		this.#target = value;
	}

	listen(effect: Effect, fn: (frame: AudioFrame) => void): void {
		this.#listeners.add(fn);
		effect.cleanup(() => this.#listeners.delete(fn));
	}

	close(): void {
		// Resolves the pending read with `done`, which ends the pump.
		void this.#reader.cancel();
	}

	async #pump(): Promise<void> {
		for (;;) {
			const { value } = await this.#reader.read();
			if (!value) break;

			const frame: AudioFrame = {
				timestamp: value.timestamp as Time.Micro,
				channels: planar(value),
			};
			value.close();

			this.#ramp(frame.channels);

			// The framer copies out of these buffers, so every listener can share them.
			for (const listener of this.#listeners) {
				// A listener that throws (a codec that died mid-stream) must not take the source
				// down with it: the encoder rebuilds, and it needs this still running to feed it.
				try {
					listener(frame);
				} catch (err) {
					console.error("audio listener failed:", err);
				}
			}
		}
	}

	// Walk the gain toward its target across the frame. Jumping straight to it would click.
	#ramp(channels: Float32Array[]): void {
		if (this.#gain === 1 && this.#target === 1) return;

		// How far the gain may move per sample to cover the full range in FADE seconds.
		const step = 1 / (FADE * this.sampleRate);
		const samples = channels[0]?.length ?? 0;

		for (let index = 0; index < samples; index++) {
			if (this.#gain < this.#target) this.#gain = Math.min(this.#target, this.#gain + step);
			else if (this.#gain > this.#target) this.#gain = Math.max(this.#target, this.#gain - step);

			if (this.#gain === 1) continue;
			for (const channel of channels) channel[index] *= this.#gain;
		}
	}
}

// Split an AudioData into one planar float buffer per channel, converting the sample format if the
// decoder handed us something else.
function planar(data: AudioData): Float32Array[] {
	const channels: Float32Array[] = [];

	for (let index = 0; index < data.numberOfChannels; index++) {
		const plane = new Float32Array(data.numberOfFrames);
		data.copyTo(plane, { planeIndex: index, format: "f32-planar" });
		channels.push(plane);
	}

	return channels;
}

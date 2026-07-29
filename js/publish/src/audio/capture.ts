import * as Util from "@moq/hang/util";
import type { Time } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
// Compiled and inlined as a blob URL via vite-plugin-worklet.
import CaptureWorklet from "./capture-worklet.ts?worklet";
import {
	type CodecMime,
	isSampleSource,
	normalizeSource,
	type SampleSource,
	type Source,
	type SourceConfig,
} from "./types";

/** A chunk of planar PCM, timestamped against the shared wall clock. */
export interface AudioFrame {
	/** When the first sample was captured. */
	timestamp: Time.Micro;

	/** One buffer per channel, all the same length. */
	channels: Float32Array[];
}

/** The PCM format frames actually arrive in, which is not always the one we asked for. */
export interface Format {
	/** The rate the frames were captured or decoded at, in Hz. */
	sampleRate: number;

	/** The channel count the frames carry. */
	channelCount: number;
}

// How many capture quanta to hold before dropping. A worklet pushes on the audio thread and can't
// be told to wait, so a reader that falls this far behind loses the oldest audio rather than
// growing the queue without bound. Roughly 85ms at 48kHz.
const QUEUE = 32;

// Signals the capture reads.
export type CaptureInput = {
	// The track or decoded samples to pump PCM off.
	source: Getter<Source | undefined>;

	// Whether to hold the capture device. Decoded samples ignore this: there's no device to release,
	// and their stream is one-shot, so dropping it would mean never reading it again.
	enabled: Getter<boolean>;

	// The codec the PCM is destined for. Codecs only encode at a discrete set of rates, so this
	// picks the rate the capture graph runs at.
	codec: Getter<CodecMime>;

	// Override the capture rate in Hz. Defaults to the track's own.
	sampleRate: Getter<number | undefined>;

	// Force the captured channel count. Defaults to the track's requested count.
	channelCount: Getter<number | undefined>;
};

type CaptureOutput = {
	// The PCM to encode, replaced whenever the source changes. Single consumer: audio can't be
	// dropped on the floor the way a stale video frame can, so this is a stream, not a signal.
	frames: Signal<ReadableStream<AudioFrame> | undefined>;

	// The format the frames arrive in, or undefined while there's no capture.
	format: Signal<Format | undefined>;

	// The head of the Web Audio graph, when there is one, so callers can tap the raw capture.
	// Undefined for decoded samples, which never touch Web Audio.
	root: Signal<AudioNode | undefined>;
};

/**
 * Pumps PCM off an audio {@link Source} onto a stream the encoder reads.
 *
 * Absorbs the difference between the two kinds of source. A capture track is sampled through a Web
 * Audio graph and an AudioWorklet; decoded samples already carry their container's timestamps, so
 * they only need converting to planar buffers. Routing those through a realtime graph would
 * resample and restamp them, which is the whole thing publishing a file has to avoid.
 */
export class Capture {
	readonly in: Readonlys<CaptureInput>;

	readonly #out: CaptureOutput = {
		frames: new Signal<ReadableStream<AudioFrame> | undefined>(undefined),
		format: new Signal<Format | undefined>(undefined),
		root: new Signal<AudioNode | undefined>(undefined),
	};
	readonly out = readonlys(this.#out);

	#signals = new Effect();

	constructor(props?: Inputs<CaptureInput>) {
		this.in = {
			source: getter(props?.source),
			enabled: getter(props?.enabled ?? false),
			codec: getter(props?.codec ?? "opus"),
			sampleRate: getter(props?.sampleRate),
			channelCount: getter(props?.channelCount),
		};

		this.#signals.run(this.#run.bind(this));
	}

	#run(effect: Effect): void {
		const source = effect.get(this.in.source);
		if (!source) return;

		if (isSampleSource(source)) {
			this.#runSamples(source, effect);
			return;
		}

		this.#runTrack(normalizeSource(source), effect);
	}

	// Decoded samples need no graph and declare their own format, so this only converts them to the
	// planar shape the framer wants. Note it never reads `enabled`: muting must not tear the stream
	// down, because a one-shot stream can't be read again.
	#runSamples(source: SampleSource, effect: Effect): void {
		const { sampleRate, channelCount } = source;

		effect.set(this.#out.format, { sampleRate, channelCount }, undefined);
		effect.set(this.#out.frames, source.samples.pipeThrough(planar()), undefined);
	}

	#runTrack(source: SourceConfig, effect: Effect): void {
		// Releasing the capture device while muted is the whole point of gating on `enabled`.
		if (!effect.get(this.in.enabled)) return;

		const settings = source.track.getSettings();
		const overrideSampleRate = effect.get(this.in.sampleRate);
		const mime = effect.get(this.in.codec);
		const sampleRate = pickSampleRate(mime, overrideSampleRate ?? settings.sampleRate);

		if (overrideSampleRate !== undefined && sampleRate !== overrideSampleRate) {
			console.warn(`${mime} does not support ${overrideSampleRate}Hz, capturing at ${sampleRate}Hz`);
		}

		// macOS misreports a mono mic as stereo: getSettings().channelCount is undefined and
		// MediaStreamAudioSourceNode.channelCount defaults to 2, so the graph carries (and Opus
		// encodes) duplicated mono as stereo. Prefer an explicitly requested channel count, from
		// the prop or the track's applied getUserMedia constraint, and force the worklet to mix to it.
		const requestedChannels = effect.get(this.in.channelCount) ?? requestedChannelCount(source.track);

		const context = new AudioContext({
			latencyHint: "interactive",
			sampleRate,
		});
		effect.cleanup(() => context.close());

		const root = new MediaStreamAudioSourceNode(context, {
			mediaStream: new MediaStream([source.track]),
		});
		effect.cleanup(() => root.disconnect());

		effect.cleanup(() => {
			this.#out.format.set(undefined);
		});

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
			effect.cleanup(() => worklet.disconnect());

			root.connect(worklet);

			const frames = this.#drain(worklet, context.sampleRate, effect);

			effect.set(this.#out.root, root);
			effect.set(this.#out.frames, frames);
		});
	}

	// Turn the quanta the worklet pushes into a stream. The audio thread can't be asked to wait, so
	// a reader that falls too far behind loses the oldest audio; downstream that reads as a
	// timestamp gap, which the framer re-anchors on rather than silently sliding.
	#drain(worklet: AudioWorkletNode, sampleRate: number, effect: Effect): ReadableStream<AudioFrame> {
		return new ReadableStream<AudioFrame>(
			{
				start: (controller) => {
					effect.event(worklet.port, "message", (event: Event) => {
						const frame = (event as MessageEvent<AudioFrame>).data;
						const channelCount = frame.channels.length;
						if (!channelCount) return;

						// The channel count is unreliable on some platforms (Apple's Safari), so
						// record what actually arrives rather than what we asked for.
						if (this.#out.format.peek()?.channelCount !== channelCount) {
							this.#out.format.set({ sampleRate, channelCount });
						}

						if ((controller.desiredSize ?? 0) > 0) controller.enqueue(frame);
					});
					worklet.port.start();
				},
			},
			{ highWaterMark: QUEUE },
		);
	}

	/** Stop capturing and release the graph. */
	close(): void {
		this.#signals.close();
	}
}

// Split each AudioData into one planar float buffer per channel, converting the sample format when
// the decoder hands us something else.
function planar(): TransformStream<AudioData, AudioFrame> {
	return new TransformStream<AudioData, AudioFrame>({
		transform(data, controller) {
			const channels: Float32Array[] = [];

			for (let index = 0; index < data.numberOfChannels; index++) {
				const plane = new Float32Array(data.numberOfFrames);
				data.copyTo(plane, { planeIndex: index, format: "f32-planar" });
				channels.push(plane);
			}

			controller.enqueue({ timestamp: data.timestamp as Time.Micro, channels });
			data.close();
		},
	});
}

// getConstraints() echoes the constraints applied via getUserMedia, which (unlike getSettings)
// survives the macOS mono->stereo misreport. Returns the requested channel count, if any.
function requestedChannelCount(track: MediaStreamTrack): number | undefined {
	const constraint = track.getConstraints().channelCount;
	if (constraint === undefined) return undefined;
	if (typeof constraint === "number") return constraint;
	return constraint.exact ?? constraint.ideal ?? constraint.max ?? constraint.min;
}

/**
 * Pick the rate to run the capture graph at, given what the source reports (or the caller asked
 * for). Codecs only encode at a discrete set of rates, so snap to one they actually support.
 *
 * This has to happen at the AudioContext rather than just in the catalog. A context running at
 * 44100 hands 44100 AudioData to an encoder configured for 48000, which WebCodecs rejects outright.
 * Snapping here keeps one rate across the capture graph, the encoder config, and the catalog.
 */
export function pickSampleRate(mime: CodecMime, requested: number | undefined): number | undefined {
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

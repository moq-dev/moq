import * as Util from "@moq/hang/util";
import type { Time } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
// Compiled and inlined as a blob URL via vite-plugin-worklet.
import { Fanout } from "../fanout";
import CaptureWorklet from "./capture-worklet.ts?worklet";
import { isSampleSource, normalizeSource, type SampleSource, type Source, type SourceConfig } from "./types";

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

// How many capture quanta a rendition may fall behind before it starts losing the oldest. A worklet
// pushes on the audio thread and can't be told to wait. Roughly 85ms at 48kHz.
const QUEUE = 32;

// The rate to run the capture graph at when nothing asks for another.
//
// Fixed rather than derived from the codec, because one capture feeds every rendition and they can
// encode differently. 48kHz is the one rate both Opus and AAC take, so the usual case needs no
// conversion at all, and Web Audio resamples the device to it far better than we could.
const DEFAULT_SAMPLE_RATE = 48_000;

// Signals the capture reads.
export type CaptureInput = {
	// The track or decoded samples to pump PCM off.
	source: Getter<Source | undefined>;

	// Whether to hold the capture device. Decoded samples ignore this: there's no device to release,
	// and their stream is one-shot, so dropping it would mean never reading it again.
	enabled: Getter<boolean>;
};

/** Constructor options: the wired inputs plus the live-editable format knobs. */
export type CaptureProps = Inputs<CaptureInput> & {
	// Seed a value or wire a Signal; also live-editable via the matching field.
	sampleRate?: number | Signal<number | undefined>;
	channelCount?: number | Signal<number | undefined>;
};

type CaptureOutput = {
	// The PCM to encode, replaced whenever the source changes. Subscribe for a stream of your own:
	// several renditions share one capture, and each needs every frame rather than the newest.
	frames: Signal<Fanout<AudioFrame> | undefined>;

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

	/** Override the capture rate in Hz. Defaults to the device's, snapped to one every codec takes. */
	sampleRate: Signal<number | undefined>;

	/** Force the captured channel count. Defaults to the track's requested count. */
	channelCount: Signal<number | undefined>;

	readonly #out: CaptureOutput = {
		frames: new Signal<Fanout<AudioFrame> | undefined>(undefined),
		format: new Signal<Format | undefined>(undefined),
		root: new Signal<AudioNode | undefined>(undefined),
	};
	readonly out = readonlys(this.#out);

	#signals = new Effect();

	constructor(props?: CaptureProps) {
		this.in = {
			source: getter(props?.source),
			enabled: getter(props?.enabled ?? false),
		};
		this.sampleRate = Signal.from<number | undefined>(props?.sampleRate);
		this.channelCount = Signal.from<number | undefined>(props?.channelCount);

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

		const fanout = new Fanout(source.samples.pipeThrough(planar()), { queue: QUEUE });
		effect.cleanup(() => fanout.close());

		effect.set(this.#out.format, { sampleRate, channelCount }, undefined);
		effect.set(this.#out.frames, fanout, undefined);
	}

	#runTrack(source: SourceConfig, effect: Effect): void {
		// Releasing the capture device while muted is the whole point of gating on `enabled`.
		if (!effect.get(this.in.enabled)) return;

		const settings = source.track.getSettings();
		const sampleRate = effect.get(this.sampleRate) ?? pickCaptureRate(settings.sampleRate);

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

			const fanout = new Fanout(this.#drain(worklet, context.sampleRate, effect), { queue: QUEUE });
			effect.cleanup(() => fanout.close());

			effect.set(this.#out.root, root);
			effect.set(this.#out.frames, fanout);
		});
	}

	// Turn the quanta the worklet pushes into a stream. The audio thread can't be asked to wait, so
	// this never applies backpressure; the fanout above bounds each reader instead. A drop shows up
	// downstream as a timestamp gap, which the framer re-anchors on rather than silently sliding.
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

// Pick the rate to run the capture graph at, given what the device reports.
//
// Every rate Opus encodes at is also in the AAC table, so staying on that set means one capture can
// feed any rendition without conversion. A device reporting anything else (a Bluetooth mic drops to
// 44.1kHz after an A2DP flip) gets resampled to 48kHz by Web Audio, which does it far better than
// we would, and which keeps the catalog off a rate Opus cannot decode.
function pickCaptureRate(reported: number | undefined): number {
	if (reported === undefined || !Number.isFinite(reported) || reported <= 0) return DEFAULT_SAMPLE_RATE;
	return Util.Opus.supportsRate(reported) ? reported : DEFAULT_SAMPLE_RATE;
}

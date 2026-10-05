import { Time } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import { Fanout } from "../fanout";
import { TrackProcessor } from "./processor";
import { normalizeSource, type Source } from "./types";

// The raw capture source to pump frames from.
export type CaptureInput = {
	source: Getter<Source | undefined>;
};

type CaptureOutput = {
	// The captured frames, replaced whenever the source changes. Subscribe for a stream of your own,
	// starting with a copy of the current picture; each reader owns the frames it receives and must
	// close them.
	frames: Signal<Fanout<VideoFrame> | undefined>;
	// The captured dimensions and source scale, sampled together for each frame.
	display: Signal<{ width: number; height: number; scale?: number } | undefined>;
};

/**
 * Pumps frames off a capture {@link Source} and distributes them to any number of readers.
 *
 * Split out of the encoders so one capture feeds every rendition, the preview, and the stats. Each
 * reader gets its own copy of every frame and closes what it receives; a reader that falls behind
 * loses its own oldest rather than stalling the capture. A new reader starts with the current
 * picture, so a still source (a screen share of an unchanging slide) is visible to it.
 */
export class Capture {
	readonly in: Readonlys<CaptureInput>;

	readonly #out: CaptureOutput = {
		frames: new Signal<Fanout<VideoFrame> | undefined>(undefined),
		display: new Signal<{ width: number; height: number; scale?: number } | undefined>(undefined),
	};
	readonly out = readonlys(this.#out);

	#signals = new Effect();

	constructor(props?: Inputs<CaptureInput>) {
		this.in = {
			source: getter(props?.source),
		};

		this.#signals.run(this.#run.bind(this));
	}

	#run(effect: Effect) {
		const source = effect.get(this.in.source);
		if (!source) return;

		// A capture track goes through MediaStreamTrackProcessor, which rewrites timestamps onto our
		// wall clock so they stay consistent when the source changes or the encoder reloads. A
		// FrameSource already stamps against that clock, so take its frames as they are.
		const stream = "frames" in source ? source.frames : TrackProcessor(normalizeSource(source).track);

		const fanout = new Frames(stream.pipeThrough(this.#measure(source)));
		effect.cleanup(() => fanout.close());

		effect.set(this.#out.frames, fanout, undefined);
		effect.cleanup(() => {
			this.#out.display.set(undefined);
		});
	}

	// Sample live source metadata even when the coded dimensions stay unchanged.
	#measure(source: Source): TransformStream<VideoFrame, VideoFrame> {
		return new TransformStream<VideoFrame, VideoFrame>({
			transform: (frame, controller) => {
				try {
					const scale = "frames" in source ? undefined : normalizeSource(source).scale;
					this.#out.display.set({ width: frame.codedWidth, height: frame.codedHeight, scale });
					controller.enqueue(frame);
				} catch (error) {
					frame.close();
					throw error;
				}
			},
		});
	}

	close() {
		this.#signals.close();
	}
}

// A fanout that holds the newest frame and opens every new reader with a copy of it. A still source
// delivers a frame only when its picture changes, so without this a reader that attaches later (a
// late viewer, an encoder resuming after a demand gap) waits for a change that may never come.
// Video only: replaying audio would repeat sound.
class Frames extends Fanout<VideoFrame> {
	// The newest frame through, owned here and dropped once closed. Boxed because the tap that fills
	// it is built before super().
	readonly #held: Held;

	constructor(source: ReadableStream<VideoFrame>) {
		const held: Held = { closed: false };
		const hold = new TransformStream<VideoFrame, VideoFrame>({
			transform: (frame, controller) => {
				held.frame?.close();
				held.frame = held.closed ? undefined : frame.clone();
				controller.enqueue(frame);
			},
		});

		super(source.pipeThrough(hold), {
			// A frame is a resource with an explicit lifetime, so every reader needs its own handle
			// and closes it. Sharing one would let the first reader close it under the others.
			clone: (frame) => frame.clone(),
			release: (frame) => frame.close(),
		});

		this.#held = held;
	}

	override subscribe(effect: Effect, queue?: number): ReadableStream<VideoFrame> {
		const live = super.subscribe(effect, queue);
		const held = this.#held.frame;
		if (!held) return live;

		// Re-stamped to now. The held frame may be minutes old, and publishing it at its capture time
		// would deliver it that late, a delay the jitter estimate keeps for the life of the stream.
		const at = Time.Micro.fromMilli(performance.now() as Time.Milli);
		let first: VideoFrame | undefined = new VideoFrame(held, { timestamp: at });
		const release = () => {
			first?.close();
			first = undefined;
		};
		effect.cleanup(release);

		const reader = live.getReader();
		return new ReadableStream<VideoFrame>(
			{
				pull: async (controller) => {
					if (first) {
						controller.enqueue(first);
						first = undefined;
						return;
					}

					for (;;) {
						const { value } = await reader.read();
						if (!value) {
							controller.close();
							return;
						}

						// A frame captured just before the copy's stamp would run time backwards.
						if (value.timestamp > at) {
							controller.enqueue(value);
							return;
						}

						value.close();
					}
				},
				cancel: async (reason) => {
					release();
					await reader.cancel(reason);
				},
			},
			// Pull on demand, so the fanout's queue stays the only buffer and its drop policy holds.
			{ highWaterMark: 0 },
		);
	}

	override close(): void {
		super.close();
		this.#held.closed = true;
		this.#held.frame?.close();
		this.#held.frame = undefined;
	}
}

type Held = { frame?: VideoFrame; closed: boolean };

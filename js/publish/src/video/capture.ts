import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import { Fanout } from "../fanout";
import { TrackProcessor } from "./processor";
import { isStreamTrack, type Source } from "./types";

// The raw capture source to pump frames from.
export type CaptureInput = {
	source: Getter<Source | undefined>;
};

type CaptureOutput = {
	// The captured frames, replaced whenever the source changes. Subscribe for a stream of your own;
	// each reader owns the frames it receives and must close them.
	frames: Signal<Fanout<VideoFrame> | undefined>;
	// The captured (coded) dimensions, tracked from each frame.
	display: Signal<{ width: number; height: number } | undefined>;
};

/**
 * Pumps frames off a capture {@link Source} and distributes them to any number of readers.
 *
 * Split out of the encoders so one capture feeds every rendition, the preview, and the stats. Each
 * reader gets its own copy of every frame and closes what it receives; a reader that falls behind
 * loses its own oldest rather than stalling the capture.
 */
export class Capture {
	readonly in: Readonlys<CaptureInput>;

	readonly #out: CaptureOutput = {
		frames: new Signal<Fanout<VideoFrame> | undefined>(undefined),
		display: new Signal<{ width: number; height: number } | undefined>(undefined),
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
		const stream = isStreamTrack(source) ? TrackProcessor(source) : source.frames;

		const fanout = new Fanout(stream.pipeThrough(this.#measure()), {
			// A frame is a resource with an explicit lifetime, so every reader needs its own handle
			// and closes it. Sharing one would let the first reader close it under the others.
			clone: (frame) => frame.clone(),
			release: (frame) => frame.close(),
		});
		effect.cleanup(() => fanout.close());

		effect.set(this.#out.frames, fanout, undefined);
		effect.cleanup(() => {
			this.#out.display.set(undefined);
		});
	}

	// Read the coded size off each frame on its way past. The signal only notifies when the size
	// actually changes, so this costs nothing per frame beyond the comparison.
	#measure(): TransformStream<VideoFrame, VideoFrame> {
		return new TransformStream<VideoFrame, VideoFrame>({
			transform: (frame, controller) => {
				this.#out.display.set({ width: frame.codedWidth, height: frame.codedHeight });
				controller.enqueue(frame);
			},
		});
	}

	close() {
		this.#signals.close();
	}
}

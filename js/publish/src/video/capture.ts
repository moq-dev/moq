import { Instant, Nano, now } from "@moq/clock";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import { Fanout } from "../fanout";
import { TrackProcessor } from "./processor";
import { normalizeSource, type Source } from "./types";

// Signals the capture reads.
export type CaptureInput = {
	// The raw capture source to pump frames from.
	source: Getter<Source | undefined>;
	// Clockwise turn a landscape frame needs to appear upright, usually a Camera's `orientation`.
	// Ignored for portrait frames and for frames that carry their own rotation. Defaults to 0.
	orientation: Getter<number>;
};

type CaptureOutput = {
	// The captured frames, replaced whenever the source changes. Subscribe for a stream of your own,
	// starting with a copy of the current picture; each reader owns the frames it receives and must
	// close them.
	frames: Signal<Fanout<VideoFrame> | undefined>;
	// The captured dimensions and source scale, sampled together for each frame.
	display: Signal<{ width: number; height: number; scale?: number } | undefined>;
	// Clockwise rotation needed to show the live frames upright. Frames never carry their own
	// VideoFrame.rotation: it is moved here, so every reader applies exactly one turn.
	rotation: Signal<number>;
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
		rotation: new Signal(0),
	};
	readonly out = readonlys(this.#out);

	// The turn the newest frame declared via VideoFrame.rotation, or undefined where unsupported.
	#native = new Signal<number | undefined>(undefined);

	#signals = new Effect();

	constructor(props?: Inputs<CaptureInput>) {
		this.in = {
			source: getter(props?.source),
			orientation: getter(props?.orientation ?? 0),
		};

		this.#signals.run(this.#run.bind(this));
		this.#signals.run(this.#runRotation.bind(this));
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
			this.#native.set(undefined);
		});
	}

	// Derived from the live frames rather than the track settings: Safari reports the requested size
	// until the first frame arrives, then the delivered size without an event.
	#runRotation(effect: Effect) {
		const display = effect.get(this.#out.display);
		if (!display) return;

		// A browser that exposes VideoFrame.rotation describes the frame fully, even when it says 0.
		const native = effect.get(this.#native);
		const landscape = display.width > display.height;
		const rotation = native ?? (landscape ? effect.get(this.in.orientation) : 0);
		effect.set(this.#out.rotation, rotation, 0);
	}

	// Sample live source metadata even when the coded dimensions stay unchanged.
	#measure(source: Source): TransformStream<VideoFrame, VideoFrame> {
		return new TransformStream<VideoFrame, VideoFrame>({
			transform: (input, controller) => {
				let frame = input;
				try {
					const scale = "frames" in source ? undefined : normalizeSource(source).scale;
					this.#out.display.set({ width: frame.codedWidth, height: frame.codedHeight, scale });

					// Readers would otherwise turn a tagged frame twice: once natively when drawing, again
					// for `out.rotation`. The encoder would also copy it into its decoder config. A
					// constructor rotation adds to the frame's own, so cancel it here.
					const tagged = frame as VideoFrame & { rotation?: number; flip?: boolean };
					let native = tagged.rotation;
					if (native) {
						// A mirrored frame subtracts a constructor rotation, and its mirror, drawn natively first,
						// makes the picture need the opposite turn.
						frame = new VideoFrame(input, {
							rotation: tagged.flip ? native : 360 - native,
						} as VideoFrameInit);
						input.close();
						if (tagged.flip) native = 360 - native;
					}
					this.#native.set(native);

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

	// Copies handed to new readers and not yet read, released on close like the fanout's own queues.
	readonly #unread = new Set<VideoFrame>();

	// The latest stamp handed to a new reader. Live frames follow the source's order, but a replay is
	// stamped here, and a later frame can be held below it.
	#replayed = 0;

	constructor(source: ReadableStream<VideoFrame>) {
		const held: Held = { closed: false, at: Instant(0) };
		const hold = new TransformStream<VideoFrame, VideoFrame>({
			transform: (frame, controller) => {
				held.frame?.close();
				held.frame = held.closed ? undefined : frame.clone();
				held.at = now();
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
		// Capture stamps can run ahead of the clock (the first frame's arrival anchors them), so now
		// alone could fall below frames an earlier reader published, rewinding a track resumed after a
		// gap. Never stamp below the held stamp plus how long it was held, nor below an earlier replay.
		const clock = now();
		const elapsed = Nano.toMicro(Instant.durationSince(clock, this.#held.at));
		const at = Math.round(Math.max(held.timestamp + elapsed, Nano.toMicro(Nano(clock)), this.#replayed));
		this.#replayed = at;
		let first: VideoFrame | undefined = new VideoFrame(held, { timestamp: at });
		this.#unread.add(first);
		const release = () => {
			if (first && this.#unread.delete(first)) first.close();
			first = undefined;
		};
		effect.cleanup(release);

		const reader = live.getReader();
		return new ReadableStream<VideoFrame>(
			{
				pull: async (controller) => {
					const copy = first;
					first = undefined;
					// Already released if the capture closed first.
					if (copy && this.#unread.delete(copy)) {
						controller.enqueue(copy);
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

		for (const frame of this.#unread) frame.close();
		this.#unread.clear();
	}
}

// The newest frame, when it came through, and whether the fanout closed.
type Held = { frame?: VideoFrame; at: Instant; closed: boolean };

import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import { TrackProcessor } from "./polyfill";
import type { Source } from "./types";

// The raw capture source to pump frames from.
export type CaptureInput = {
	source: Getter<Source | undefined>;
};

type CaptureOutput = {
	// The latest captured frame. Owned here: closed when replaced and on close().
	frame: Signal<VideoFrame | undefined>;
	// The captured (coded) dimensions, tracked from each frame.
	display: Signal<{ width: number; height: number } | undefined>;
};

/**
 * Pumps frames off a capture {@link Source} onto an owned {@link out} frame signal.
 *
 * Split out of the encoders so one capture feeds any number of renditions. The output frame is owned
 * here (closed on replace and on {@link close}); readers must not close it.
 */
export class Capture {
	readonly in: Readonlys<CaptureInput>;

	readonly #out: CaptureOutput = {
		frame: new Signal<VideoFrame | undefined>(undefined),
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

		// NOTE: We modify the stock MediaStreamTrackProcessor so timestamps use our wall clock time.
		// This is so even when the source is changed or encoder reloaded, the timestamps will be consistent.
		const reader = TrackProcessor(source).getReader();
		effect.cleanup(() => reader.cancel());

		effect.spawn(async () => {
			for (;;) {
				const next = await Promise.race([reader.read(), effect.cancel]);
				if (!next?.value) break;

				this.#out.frame.update((prev) => {
					prev?.close();
					return next.value;
				});

				this.#out.display.set({ width: next.value.codedWidth, height: next.value.codedHeight });
			}
		});

		effect.cleanup(() => {
			this.#out.frame.update((prev) => {
				prev?.close();
				return undefined;
			});
			this.#out.display.set(undefined);
		});
	}

	close() {
		this.#signals.close();

		this.#out.frame.update((prev) => {
			prev?.close();
			return undefined;
		});
	}
}

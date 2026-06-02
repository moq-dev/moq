import { Time } from "@moq/net";
import { Effect, type Getter, Signal } from "@moq/signals";
import type { Decoder } from "./decoder";

export type RendererProps = {
	canvas?: HTMLCanvasElement | Signal<HTMLCanvasElement | undefined>;

	// Whether to paint decoded frames. When false the canvas is blanked and the
	// last frame released. This gates display only; download gating lives on the
	// Decoder's own `enabled`.
	enabled?: boolean | Signal<boolean>;
};

// An component to render a video to a canvas.
export class Renderer {
	decoder: Decoder;

	// The canvas to render the video to.
	canvas: Signal<HTMLCanvasElement | undefined>;

	// Whether to paint frames (true) or blank the canvas (false).
	enabled: Signal<boolean>;

	// The most recently rendered frame, updated after each rAF paint.
	readonly frame = new Signal<VideoFrame | undefined>(undefined);

	// The media timestamp of the most recently rendered frame.
	readonly timestamp = new Signal<Time.Milli | undefined>(undefined);

	// Whether a real frame is currently painted. A download gate can watch this
	// to stop once a paused poster is on screen, keyed off the actual paint (post-rAF).
	#rendered = new Signal(false);
	readonly rendered: Getter<boolean> = this.#rendered;

	#ctx = new Signal<CanvasRenderingContext2D | undefined>(undefined);
	#signals = new Effect();

	constructor(decoder: Decoder, props?: RendererProps) {
		this.decoder = decoder;
		this.canvas = Signal.from(props?.canvas);
		this.enabled = Signal.from(props?.enabled ?? true);

		this.#signals.run((effect) => {
			const canvas = effect.get(this.canvas);
			this.#ctx.set(canvas?.getContext("2d") ?? undefined);
		});

		this.#signals.run(this.#runRender.bind(this));
		this.#signals.run(this.#runResize.bind(this));
	}

	#runResize(effect: Effect) {
		const values = effect.getAll([this.canvas, this.decoder.display]);
		if (!values) return; // Keep current canvas size until we have new dimensions
		const [canvas, display] = values;

		// Only update if dimensions actually changed (setting canvas.width/height clears the canvas)
		// TODO I thought the signals library would prevent this, but I'm too lazy to investigate.
		if (canvas.width !== display.width || canvas.height !== display.height) {
			canvas.width = display.width;
			canvas.height = display.height;
		}
	}

	#runRender(effect: Effect) {
		const ctx = effect.get(this.#ctx);
		if (!ctx) return;

		// When disabled, blank the canvas and release the last frame instead of
		// painting. The decoder keeps its frame, so re-enabling repaints it.
		const enabled = effect.get(this.enabled);
		const frame = enabled ? effect.get(this.decoder.frame) : undefined;

		// Request a callback to render the frame based on the monitor's refresh rate.
		let animate: number | undefined = requestAnimationFrame(() => {
			this.#render(ctx, frame);

			this.frame.update((current) => {
				current?.close();
				return frame?.clone();
			});
			this.timestamp.set(frame ? Time.Milli.fromMicro(frame.timestamp as Time.Micro) : undefined);
			this.#rendered.set(!!frame);

			animate = undefined;
		});

		// Clean up any pending animation request.
		effect.cleanup(() => {
			if (animate) cancelAnimationFrame(animate);
		});
	}

	#render(ctx: CanvasRenderingContext2D, frame?: VideoFrame) {
		if (!frame) {
			// Clear canvas when no frame
			ctx.fillStyle = "#000";
			ctx.fillRect(0, 0, ctx.canvas.width, ctx.canvas.height);
			return;
		}

		// Prepare background and transformations for this draw
		ctx.save();
		ctx.fillStyle = "#000";
		ctx.fillRect(0, 0, ctx.canvas.width, ctx.canvas.height);

		// Apply horizontal flip if specified in the video config
		const flip = this.decoder.source.catalog.peek()?.flip;
		if (flip) {
			ctx.scale(-1, 1);
			ctx.translate(-ctx.canvas.width, 0);
		}

		ctx.drawImage(frame, 0, 0, ctx.canvas.width, ctx.canvas.height);
		ctx.restore();
	}

	// Close the track and all associated resources.
	close() {
		this.frame.update((current) => {
			current?.close();
			return undefined;
		});
		this.timestamp.set(undefined);
		this.#signals.close();
	}
}

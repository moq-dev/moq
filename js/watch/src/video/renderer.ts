import { Effect, Signal } from "@moq/signals";
import type { Decoder } from "./decoder";

export type RendererProps = {
	canvas?: HTMLCanvasElement | Signal<HTMLCanvasElement | undefined>;
	paused?: boolean | Signal<boolean>;
};

// An component to render a video to a canvas.
export class Renderer {
	decoder: Decoder;

	// The canvas to render the video to.
	canvas: Signal<HTMLCanvasElement | undefined>;

	// Whether the video is paused.
	paused: Signal<boolean>;

	// Cache the last rendered frame to keep it visible when paused
	#lastFrame?: VideoFrame;

	#ctx = new Signal<CanvasRenderingContext2D | undefined>(undefined);
	#signals = new Effect();

	constructor(decoder: Decoder, props?: RendererProps) {
		this.decoder = decoder;
		this.canvas = Signal.from(props?.canvas);
		this.paused = Signal.from(props?.paused ?? false);

		this.#signals.run((effect) => {
			const canvas = effect.get(this.canvas);
			this.#ctx.set(canvas?.getContext("2d") ?? undefined);
		});

		this.#signals.run(this.#runEnabled.bind(this));
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

	// Detect when video should be downloaded.
	#runEnabled(effect: Effect): void {
		const canvas = effect.get(this.canvas);
		if (!canvas) return;

		const paused = effect.get(this.paused);
		if (paused) return;

		// Detect when the canvas is not visible.
		const observer = new IntersectionObserver(
			(entries) => {
				for (const entry of entries) {
					this.decoder.enabled.set(entry.isIntersecting);
				}
			},
			{
				// fire when even a small part is visible
				threshold: 0.01,
			},
		);

		effect.cleanup(() => this.decoder.enabled.set(false));

		observer.observe(canvas);
		effect.cleanup(() => observer.disconnect());
	}

	#runRender(effect: Effect) {
		const ctx = effect.get(this.#ctx);
		if (!ctx) return;

		let frame: VideoFrame | undefined;

		const paused = effect.get(this.paused);
		if (!paused) {
			frame = effect.get(this.decoder.frame);
			this.#lastFrame?.close();
			this.#lastFrame = frame?.clone();
		} else {
			frame = this.#lastFrame?.clone();
		}

		// Request a callback to render the frame based on the monitor's refresh rate.
		// Always render, even when paused (to show last frame)
		let animate: number | undefined = requestAnimationFrame(() => {
			this.#render(ctx, frame);
			animate = undefined;
		});

		// Clean up the frame and any pending animation request.
		effect.cleanup(() => {
			// NOTE: Closing this frame is the only reason we don't use `effect.animate`.
			// It's slighly more efficient to use one .cleanup() callback instead of two.
			frame?.close();
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
		// Clean up cached frame
		this.#lastFrame?.close();
		this.#lastFrame = undefined;
		this.#signals.close();
	}
}

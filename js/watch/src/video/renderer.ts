import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import { Time } from "@moq/wasm";
import type { Decoder } from "./decoder";

type RendererInput = {
	canvas: Getter<HTMLCanvasElement | undefined>;
};

type RendererOutput = {
	// The most recently rendered frame, updated after each rAF paint.
	frame: Signal<VideoFrame | undefined>;

	// The media timestamp of the most recently rendered frame.
	timestamp: Signal<Time.Milli | undefined>;

	// Whether the canvas is visible in the viewport and the tab is focused.
	// The owner combines this with `paused` to drive the decoder's `enabled` input.
	visible: Signal<boolean>;
};

// An component to render a video to a canvas.
export class Renderer {
	decoder: Decoder;

	readonly input: Readonlys<RendererInput>;

	readonly #output: RendererOutput = {
		frame: new Signal<VideoFrame | undefined>(undefined),
		timestamp: new Signal<Time.Milli | undefined>(undefined),
		visible: new Signal(false),
	};
	readonly output = readonlys(this.#output);

	#ctx = new Signal<CanvasRenderingContext2D | undefined>(undefined);
	#signals = new Effect();

	constructor(decoder: Decoder, props?: Inputs<RendererInput>) {
		this.decoder = decoder;
		this.input = {
			canvas: getter(props?.canvas),
		};

		this.#signals.run((effect) => {
			const canvas = effect.get(this.input.canvas);
			this.#ctx.set(canvas?.getContext("2d") ?? undefined);
		});

		this.#signals.run(this.#runVisible.bind(this));
		this.#signals.run(this.#runRender.bind(this));
		this.#signals.run(this.#runResize.bind(this));
	}

	#runResize(effect: Effect) {
		const values = effect.getAll([this.input.canvas, this.decoder.output.display]);
		if (!values) return; // Keep current canvas size until we have new dimensions
		const [canvas, display] = values;

		// Only update if dimensions actually changed (setting canvas.width/height clears the canvas)
		// TODO I thought the signals library would prevent this, but I'm too lazy to investigate.
		if (canvas.width !== display.width || canvas.height !== display.height) {
			canvas.width = display.width;
			canvas.height = display.height;
		}
	}

	// Track whether the canvas is visible in the viewport and the tab is focused.
	#runVisible(effect: Effect): void {
		const canvas = effect.get(this.input.canvas);
		if (!canvas) {
			this.#output.visible.set(false);
			return;
		}

		let intersecting = false;

		const update = () => {
			this.#output.visible.set(intersecting && !document.hidden);
		};

		const observer = new IntersectionObserver(
			(entries) => {
				for (const entry of entries) {
					intersecting = entry.isIntersecting;
					update();
				}
			},
			{ threshold: 0.01 },
		);

		effect.event(document, "visibilitychange", update);

		observer.observe(canvas);
		effect.cleanup(() => observer.disconnect());
		effect.cleanup(() => this.#output.visible.set(false));
	}

	#runRender(effect: Effect) {
		const ctx = effect.get(this.#ctx);
		if (!ctx) return;

		const frame = effect.get(this.decoder.output.frame);

		// Request a callback to render the frame based on the monitor's refresh rate.
		// Always render, even when paused (to show last frame).
		let animate: number | undefined = requestAnimationFrame(() => {
			this.#render(ctx, frame);

			if (frame) {
				this.#output.frame.update((current) => {
					current?.close();
					return frame.clone();
				});
				this.#output.timestamp.set(Time.Milli.fromMicro(frame.timestamp as Time.Micro));
			} else {
				this.#output.frame.update((current) => {
					current?.close();
					return undefined;
				});
				this.#output.timestamp.set(undefined);
			}

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
		const flip = this.decoder.source.output.catalog.peek()?.flip;
		if (flip) {
			ctx.scale(-1, 1);
			ctx.translate(-ctx.canvas.width, 0);
		}

		ctx.drawImage(frame, 0, 0, ctx.canvas.width, ctx.canvas.height);
		ctx.restore();
	}

	// Close the track and all associated resources.
	close() {
		this.#output.frame.update((current) => {
			current?.close();
			return undefined;
		});
		this.#output.timestamp.set(undefined);
		this.#signals.close();
	}
}

import { Time } from "@moq/lite";
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

	// The most recently rendered frame, updated after each rAF paint.
	readonly frame = new Signal<VideoFrame | undefined>(undefined);

	// The media timestamp of the most recently rendered frame.
	readonly timestamp = new Signal<Time.Milli | undefined>(undefined);

	#ctx = new Signal<CanvasRenderingContext2D | undefined>(undefined);
	#visible = new Signal(false);
	#signals = new Effect();

	constructor(decoder: Decoder, props?: RendererProps) {
		this.decoder = decoder;
		this.canvas = Signal.from(props?.canvas);
		this.paused = Signal.from(props?.paused ?? false);

		this.#signals.run((effect) => {
			const canvas = effect.get(this.canvas);
			this.#ctx.set(canvas?.getContext("2d") ?? undefined);
		});

		this.#signals.run(this.#runVisible.bind(this));
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

	// Track whether the canvas is visible in the viewport and the tab is focused.
	#runVisible(effect: Effect): void {
		const canvas = effect.get(this.canvas);
		if (!canvas) {
			this.#visible.set(false);
			return;
		}

		let intersecting = false;

		const update = () => {
			this.#visible.set(intersecting && !document.hidden);
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
		effect.cleanup(() => this.#visible.set(false));
	}

	// Detect when video should be downloaded.
	#runEnabled(effect: Effect): void {
		const paused = effect.get(this.paused);
		const visible = effect.get(this.#visible);

		effect.cleanup(() => this.decoder.enabled.set(false));

		if (!paused) {
			this.decoder.enabled.set(visible);
			return;
		}

		// When paused, fetch a single preview frame then disable.
		const frame = effect.get(this.frame);
		this.decoder.enabled.set(!frame);
	}

	#runRender(effect: Effect) {
		const ctx = effect.get(this.#ctx);
		if (!ctx) return;

		const paused = effect.get(this.paused);

		// Read new frames from the decoder when not paused.
		let decoded: VideoFrame | undefined;
		if (!paused) {
			decoded = effect.get(this.decoder.frame);
		}

		// Request a callback to render the frame based on the monitor's refresh rate.
		// Always render, even when paused (to show last frame)
		let animate: number | undefined = requestAnimationFrame(() => {
			const frame = decoded ?? this.frame.peek();
			this.#render(ctx, frame);

			// Update signals to reflect what's actually on screen.
			if (decoded) {
				this.frame.update((old) => {
					old?.close();
					return decoded.clone();
				});
				this.timestamp.set(Time.Milli.fromMicro(decoded.timestamp as Time.Micro));
			}

			animate = undefined;
		});

		// Clean up any pending animation request.
		effect.cleanup(() => {
			decoded?.close();
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
		this.frame.update((old) => {
			old?.close();
			return undefined;
		});
		this.#signals.close();
	}
}

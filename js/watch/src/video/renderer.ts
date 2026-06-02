import { Time } from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import type { Decoder } from "./decoder";

export type RendererProps = {
	canvas?: HTMLCanvasElement | Signal<HTMLCanvasElement | undefined>;
	paused?: boolean | Signal<boolean>;
};

// A component to render a video to a canvas.
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

			// Setting width/height blanks the canvas. Repaint the cached frame so a
			// resize while paused (decoder off, no new frames coming) doesn't leave
			// the canvas black.
			const ctx = this.#ctx.peek();
			const frame = this.frame.peek();
			if (ctx && frame) this.#draw(ctx, frame);
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

		// When the canvas (and therefore ctx) is replaced, the new canvas starts
		// blank. Paint the cached frame once so a swap while paused isn't black
		// until playback resumes.
		const cached = this.frame.peek();
		if (cached) this.#draw(ctx, cached);

		let rafId: number | undefined;

		const tick = () => {
			const frame = this.decoder.consume();
			if (frame) {
				this.#draw(ctx, frame);
				this.frame.update((old) => {
					old?.close();
					return frame; // transfer ownership from consume()
				});
				this.timestamp.set(Time.Milli.fromMicro(frame.timestamp as Time.Micro));
			}

			rafId = requestAnimationFrame(tick);
		};

		rafId = requestAnimationFrame(tick);

		effect.cleanup(() => {
			if (rafId !== undefined) cancelAnimationFrame(rafId);
		});
	}

	#draw(ctx: CanvasRenderingContext2D, frame: VideoFrame) {
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

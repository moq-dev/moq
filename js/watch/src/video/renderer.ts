import { Time } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type { Decoder } from "./decoder";

// Fraction of the canvas that must intersect the viewport before it counts as visible.
const INTERSECTION_THRESHOLD = 0.01;

/**
 * Controls when video is downloaded relative to the canvas position.
 *
 * - `"never"`: never download video.
 * - `"always"`: always download video, regardless of the canvas position or tab visibility.
 * - a CSS length (`"0px"`, `"200px"`, `"100%"`, ...): download while the canvas is within
 *   that distance of the viewport (used as the {@link IntersectionObserver} `rootMargin`) and
 *   the tab is visible. `"0px"` means strictly on screen; larger values pre-warm the video
 *   before it scrolls in.
 */
export type Visible = "never" | "always" | (string & {});

export type RendererInput = {
	canvas: Getter<HTMLCanvasElement | undefined>;

	// When video is downloaded relative to the canvas position. See {@link Visible}. Defaults to "20%".
	visible: Getter<Visible>;
};

type RendererOutput = {
	// The most recently rendered frame, updated after each rAF paint.
	frame: Signal<VideoFrame | undefined>;

	// The media timestamp of the most recently rendered frame.
	timestamp: Signal<Time.Milli | undefined>;

	// Whether the canvas should currently download per the configured distance and tab focus.
	// The owner combines this with `paused` to drive the decoder's `enabled` input.
	visible: Signal<boolean>;
};

// An component to render a video to a canvas.
export class Renderer {
	readonly decoder: Decoder;

	readonly in: Readonlys<RendererInput>;

	readonly #out: RendererOutput = {
		frame: new Signal<VideoFrame | undefined>(undefined),
		timestamp: new Signal<Time.Milli | undefined>(undefined),
		visible: new Signal(false),
	};
	readonly out = readonlys(this.#out);

	#ctx = new Signal<CanvasRenderingContext2D | undefined>(undefined);
	#signals = new Effect();

	constructor(decoder: Decoder, props?: Inputs<RendererInput>) {
		this.decoder = decoder;
		this.in = {
			canvas: getter(props?.canvas),
			visible: getter(props?.visible ?? "20%"),
		};

		this.#signals.run((effect) => {
			const canvas = effect.get(this.in.canvas);
			this.#ctx.set(canvas?.getContext("2d") ?? undefined);
		});

		this.#signals.run(this.#runVisible.bind(this));
		this.#signals.run(this.#runRender.bind(this));
		this.#signals.run(this.#runResize.bind(this));
	}

	#runResize(effect: Effect) {
		const values = effect.getAll([this.in.canvas, this.decoder.out.display]);
		if (!values) return; // Keep current canvas size until we have new dimensions
		const [canvas, display] = values;

		// Only update if dimensions actually changed (setting canvas.width/height clears the canvas)
		// TODO I thought the signals library would prevent this, but I'm too lazy to investigate.
		if (canvas.width !== display.width || canvas.height !== display.height) {
			canvas.width = display.width;
			canvas.height = display.height;
		}
	}

	// Track whether the canvas should currently download per the configured distance and tab focus.
	#runVisible(effect: Effect): void {
		const visible = effect.get(this.in.visible);

		// "never" forces the check off; "always" forces it on regardless of viewport or tab state.
		if (visible === "never") {
			this.#out.visible.set(false);
			return;
		}

		if (visible === "always") {
			this.#out.visible.set(true);
			effect.cleanup(() => this.#out.visible.set(false));
			return;
		}

		// A distance gates on the viewport (used as the rootMargin) and the tab being visible.
		const canvas = effect.get(this.in.canvas);
		if (!canvas) {
			this.#out.visible.set(false);
			return;
		}

		let intersecting = false;
		const update = () => {
			this.#out.visible.set(intersecting && !document.hidden);
		};

		const callback = (entries: IntersectionObserverEntry[]) => {
			for (const entry of entries) {
				intersecting = entry.isIntersecting;
				update();
			}
		};

		// `visible` is a CSS length, but the programmatic API accepts arbitrary strings. An
		// invalid rootMargin throws a SyntaxError, so fall back to the default margin.
		let observer: IntersectionObserver;
		try {
			observer = new IntersectionObserver(callback, { threshold: INTERSECTION_THRESHOLD, rootMargin: visible });
		} catch {
			console.warn(`moq-watch: invalid visible margin "${visible}", using "0px"`);
			observer = new IntersectionObserver(callback, { threshold: INTERSECTION_THRESHOLD });
		}

		update();
		effect.event(document, "visibilitychange", update);
		observer.observe(canvas);
		effect.cleanup(() => observer.disconnect());
		effect.cleanup(() => this.#out.visible.set(false));
	}

	#runRender(effect: Effect) {
		const ctx = effect.get(this.#ctx);
		if (!ctx) return;

		const frame = effect.get(this.decoder.out.frame);

		// Request a callback to render the frame based on the monitor's refresh rate.
		// Always render, even when paused (to show last frame).
		let animate: number | undefined = requestAnimationFrame(() => {
			this.#render(ctx, frame);

			if (frame) {
				this.#out.frame.update((current) => {
					current?.close();
					return frame.clone();
				});
				this.#out.timestamp.set(Time.Milli.fromMicro(frame.timestamp as Time.Micro));
			} else {
				this.#out.frame.update((current) => {
					current?.close();
					return undefined;
				});
				this.#out.timestamp.set(undefined);
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
		const flip = this.decoder.source.out.catalog.peek()?.flip;
		if (flip) {
			ctx.scale(-1, 1);
			ctx.translate(-ctx.canvas.width, 0);
		}

		ctx.drawImage(frame, 0, 0, ctx.canvas.width, ctx.canvas.height);
		ctx.restore();
	}

	// Close the track and all associated resources.
	close() {
		this.#out.frame.update((current) => {
			current?.close();
			return undefined;
		});
		this.#out.timestamp.set(undefined);
		this.#signals.close();
	}
}

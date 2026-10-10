import { Time } from "@moq/net";
import { Derived, Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import * as Video from "@moq/video";
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

	// Which graphics API draws the frames. See {@link Video.Backend}. Defaults to "auto".
	backend: Getter<Video.Backend>;
};

/** Constructor properties for {@link Renderer}. */
export type RendererProps = Inputs<RendererInput> & {
	/** Decoder supplying video frames. */
	decoder: Decoder;
};

// An component to render a video to a canvas.
export class Renderer {
	readonly decoder: Decoder;

	readonly in: Readonlys<RendererInput>;

	// Whether the canvas should currently download per the configured distance and tab focus.
	// The owner combines this with `paused` to drive the decoder's `enabled` input.
	#visible = new Signal(false);

	#video: Video.Renderer;

	readonly out: Readonlys<{
		// The most recently rendered frame, updated after each rAF paint.
		frame: Getter<VideoFrame | undefined>;
		// The media timestamp of the most recently rendered frame.
		timestamp: Getter<Time.Milli | undefined>;
		// Whether the canvas should currently download per the configured distance and tab focus.
		visible: Getter<boolean>;
		// Why drawing stopped, or undefined while healthy. See {@link Video.RendererError}.
		error: Getter<Video.RendererError | undefined>;
	}>;

	#signals = new Effect();

	constructor(props: RendererProps) {
		this.decoder = props.decoder;
		this.in = {
			canvas: getter(props?.canvas),
			visible: getter(props?.visible ?? "20%"),
			backend: getter(props?.backend ?? "auto"),
		};

		this.#video = new Video.Renderer({
			canvas: this.in.canvas,
			frame: this.decoder.out.frame,
			display: this.decoder.out.display,
			presentation: this.decoder.source.out.catalog,
			backend: this.in.backend,
		});
		this.#signals.cleanup(() => this.#video.close());

		const frame = this.#video.out.frame;
		this.out = readonlys({
			frame,
			timestamp: new Derived([frame], (frame) =>
				frame ? Time.Milli.fromMicro(frame.timestamp as Time.Micro) : undefined,
			),
			visible: this.#visible,
			error: this.#video.out.error,
		});

		this.#signals.run(this.#runVisible.bind(this));
	}

	// Track whether the canvas should currently download per the configured distance and tab focus.
	#runVisible(effect: Effect): void {
		const visible = effect.get(this.in.visible);

		// "never" forces the check off; "always" forces it on regardless of viewport or tab state.
		if (visible === "never") {
			this.#visible.set(false);
			return;
		}

		if (visible === "always") {
			this.#visible.set(true);
			effect.cleanup(() => this.#visible.set(false));
			return;
		}

		// A distance gates on the viewport (used as the rootMargin) and the tab being visible.
		const canvas = effect.get(this.in.canvas);
		if (!canvas) {
			this.#visible.set(false);
			return;
		}

		let intersecting = false;
		const update = () => {
			this.#visible.set(intersecting && !document.hidden);
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
		effect.cleanup(() => this.#visible.set(false));
	}

	// Close the track and all associated resources.
	close() {
		this.#signals.close();
	}
}

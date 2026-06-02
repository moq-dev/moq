import { Effect, type Getter, Signal } from "@moq/signals";

export type ElementProps = {
	canvas?: HTMLCanvasElement | Signal<HTMLCanvasElement | undefined>;
};

// Observes a canvas and reports viewport facts as signals: whether it's on
// screen and how big it's laid out. Kept separate from the Renderer so download
// gating can react to real visibility instead of being entangled with painting.
export class Element {
	canvas: Signal<HTMLCanvasElement | undefined>;

	// Whether the canvas is in the viewport and the tab is focused.
	#visible = new Signal(false);
	readonly visible: Getter<boolean> = this.#visible;

	// The laid-out size of the canvas in CSS pixels (multiply by devicePixelRatio
	// for the device-pixel size). Useful for choosing a rendition to download.
	#width = new Signal(0);
	readonly width: Getter<number> = this.#width;

	#height = new Signal(0);
	readonly height: Getter<number> = this.#height;

	#signals = new Effect();

	constructor(props?: ElementProps) {
		this.canvas = Signal.from(props?.canvas);

		this.#signals.run(this.#runVisible.bind(this));
		this.#signals.run(this.#runSize.bind(this));
	}

	#runVisible(effect: Effect): void {
		const canvas = effect.get(this.canvas);
		if (!canvas) {
			this.#visible.set(false);
			return;
		}

		let intersecting = false;
		const update = () => this.#visible.set(intersecting && !document.hidden);

		const observer = new IntersectionObserver(
			(entries) => {
				for (const entry of entries) {
					intersecting = entry.isIntersecting;
				}
				update();
			},
			{ threshold: 0.01 },
		);

		effect.event(document, "visibilitychange", update);

		observer.observe(canvas);
		effect.cleanup(() => observer.disconnect());
		effect.cleanup(() => this.#visible.set(false));
	}

	#runSize(effect: Effect): void {
		const canvas = effect.get(this.canvas);
		if (!canvas) {
			this.#width.set(0);
			this.#height.set(0);
			return;
		}

		const observer = new ResizeObserver((entries) => {
			for (const entry of entries) {
				this.#width.set(entry.contentRect.width);
				this.#height.set(entry.contentRect.height);
			}
		});

		observer.observe(canvas);
		effect.cleanup(() => observer.disconnect());
	}

	close() {
		this.#signals.close();
	}
}

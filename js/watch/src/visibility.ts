import { Effect, type Getter, Signal } from "@moq/signals";

// Tracks whether an HTML element is visible to the user, combining viewport
// intersection (IntersectionObserver) with tab focus (document.visibilityState).
//
// Defaults to `visible = false` until an element is observed and at least one
// observer callback fires. Stays false whenever the element signal is undefined.
export class Visibility {
	readonly visible = new Signal<boolean>(false);

	#signals = new Effect();

	constructor(element: Getter<Element | undefined>) {
		this.#signals.run((effect) => {
			const el = effect.get(element);
			if (!el) {
				this.visible.set(false);
				return;
			}

			let intersecting = false;
			const update = () => {
				this.visible.set(intersecting && !document.hidden);
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
			observer.observe(el);
			effect.cleanup(() => observer.disconnect());
			effect.cleanup(() => this.visible.set(false));
		});
	}

	close(): void {
		this.#signals.close();
	}
}

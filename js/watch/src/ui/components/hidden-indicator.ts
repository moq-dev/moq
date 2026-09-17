import { type Effect, Signal } from "@moq/signals";
import type MoqWatch from "../../element";

/** Explains why video isn't downloading when the `visible` gate holds it back. */
export function hiddenIndicator(parent: Effect, watch: MoqWatch): HTMLElement {
	const container = document.createElement("div");
	container.className = "watch-ui__notice watch-ui__hidden-indicator";
	container.setAttribute("role", "status");
	container.setAttribute("aria-live", "polite");

	const text = document.createElement("span");
	text.className = "watch-ui__notice-text";
	container.appendChild(text);

	// The renderer folds tab visibility into one boolean, so track it separately to name the cause.
	const hidden = new Signal(document.hidden);
	parent.event(document, "visibilitychange", () => hidden.set(document.hidden));

	parent.run((effect) => {
		const gated = !effect.get(watch.renderer.out.visible);
		const playing = !effect.get(watch.controls.paused);
		const video = effect.get(watch.video.source.out.catalog) !== undefined;
		const online = effect.get(watch.broadcast.out.status) !== "offline";
		const supported = effect.get(watch.video.source.out.error) !== "unsupported";
		// `never` is a deliberate choice, not a surprise worth explaining.
		const chosen = effect.get(watch.controls.visible) === "never";
		const show = gated && playing && video && online && supported && !chosen;
		container.style.display = show ? "" : "none";
		if (!show) return;

		if (effect.get(hidden)) {
			text.textContent = 'Video paused while the tab is hidden. Set visible="always" to keep downloading.';
		} else {
			text.textContent = 'Video paused while off-screen. Set visible="always" to keep downloading.';
		}
	});

	return container;
}

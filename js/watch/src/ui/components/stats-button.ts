import type { Effect, Signal } from "@moq/signals";
import { icon, stats } from "../icons";

export function statsButton(parent: Effect, visible: Signal<boolean>): HTMLElement {
	const button = document.createElement("button");
	button.type = "button";
	button.className = "button flex--center";
	button.replaceChildren(icon(stats));

	parent.run((effect) => {
		const showing = effect.get(visible);
		button.title = showing ? "Hide stats" : "Show stats";
		button.setAttribute("aria-label", button.title);
	});

	parent.event(button, "click", () => {
		visible.update((v) => !v);
	});

	return button;
}

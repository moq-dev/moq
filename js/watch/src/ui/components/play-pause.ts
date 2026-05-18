import type { Effect } from "@moq/signals";
import type MoqWatch from "../../element";
import { icon, pause, play } from "../icons";

export function playPauseButton(parent: Effect, watch: MoqWatch): HTMLElement {
	const button = document.createElement("button");
	button.type = "button";
	button.className = "button button--playback flex-center";

	parent.run((effect) => {
		const paused = effect.get(watch.backend.paused);
		button.title = paused ? "Play" : "Pause";
		button.setAttribute("aria-label", paused ? "Play" : "Pause");
		button.replaceChildren(icon(paused ? play : pause));
	});

	parent.event(button, "click", () => {
		watch.paused = !watch.paused;
	});

	return button;
}

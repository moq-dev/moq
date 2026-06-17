import type { Effect } from "@moq/signals";
import type MoqWatch from "../../element";
import { icon, pause, play } from "../icons";
import { controlButton } from "./button";

/** Play/pause control bound to the backend paused state. */
export function playPauseButton(parent: Effect, watch: MoqWatch): HTMLElement {
	const button = controlButton(play, "Play");

	parent.run((effect) => {
		const paused = effect.get(watch.backend.paused);
		button.title = paused ? "Play" : "Pause";
		button.setAttribute("aria-label", button.title);
		button.replaceChildren(icon(paused ? play : pause));
	});

	parent.event(button, "click", () => {
		watch.paused = !watch.paused;
	});

	return button;
}

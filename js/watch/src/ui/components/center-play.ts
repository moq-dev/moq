import type { Effect } from "@moq/signals";
import type MoqWatch from "../../element";
import { icon, play } from "../icons";

/** A large centered play button shown while paused. */
export function centerPlay(parent: Effect, watch: MoqWatch): HTMLElement {
	const button = document.createElement("button");
	button.type = "button";
	button.className = "center-play flex-center";
	button.title = "Play";
	button.setAttribute("aria-label", "Play");
	button.replaceChildren(icon(play));

	parent.run((effect) => {
		const paused = effect.get(watch.backend.paused);
		button.style.display = paused ? "" : "none";
	});

	parent.event(button, "click", () => {
		watch.paused = false;
	});

	return button;
}

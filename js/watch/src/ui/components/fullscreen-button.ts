import type { Effect } from "@moq/signals";
import type MoqWatch from "../../element";
import { fullscreenEnter, fullscreenExit, icon } from "../icons";

export function fullscreenButton(parent: Effect, watch: MoqWatch): HTMLElement {
	const button = document.createElement("button");
	button.type = "button";
	button.className = "button flex-center";
	button.title = "Fullscreen";
	button.setAttribute("aria-label", "Fullscreen");
	button.replaceChildren(icon(fullscreenEnter));

	const updateIcon = () => {
		const isFull = !!document.fullscreenElement;
		button.replaceChildren(icon(isFull ? fullscreenExit : fullscreenEnter));
	};
	parent.event(document, "fullscreenchange", updateIcon);

	parent.event(button, "click", () => {
		if (document.fullscreenElement) {
			document.exitFullscreen();
		} else {
			watch.requestFullscreen();
		}
	});

	return button;
}

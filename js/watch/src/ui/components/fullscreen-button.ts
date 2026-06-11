import type { Effect } from "@moq/signals";
import type MoqWatch from "../../element";
import { fullscreenEnter, fullscreenExit, icon } from "../icons";
import { controlButton } from "./button";

export function fullscreenButton(parent: Effect, watch: MoqWatch): HTMLElement {
	const button = controlButton(fullscreenEnter, "Fullscreen");

	const updateIcon = () => {
		const isFull = document.fullscreenElement === watch;
		button.replaceChildren(icon(isFull ? fullscreenExit : fullscreenEnter));
		button.title = isFull ? "Exit fullscreen" : "Fullscreen";
		button.setAttribute("aria-label", button.title);
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

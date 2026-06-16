import type { Effect } from "@moq/signals";
import { createFullscreen } from "../fullscreen";
import { fullscreenEnter, fullscreenExit, icon } from "../icons";
import { controlButton } from "./button";

/** Fullscreen toggle button wired to the shared cross-browser fullscreen controller. */
export function fullscreenButton(parent: Effect, player: HTMLElement): HTMLElement {
	const button = controlButton(fullscreenEnter, "Fullscreen");
	const fullscreen = createFullscreen(parent, player);

	const updateIcon = () => {
		const isFull = fullscreen.active();
		button.replaceChildren(icon(isFull ? fullscreenExit : fullscreenEnter));
		button.title = isFull ? "Exit fullscreen" : "Fullscreen";
		button.setAttribute("aria-label", button.title);
	};
	updateIcon();
	parent.cleanup(fullscreen.onChange(updateIcon));

	parent.event(button, "click", () => fullscreen.toggle());

	return button;
}

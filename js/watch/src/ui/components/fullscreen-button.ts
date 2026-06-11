import type { Effect } from "@moq/signals";
import type MoqWatch from "../../element";
import { createFullscreen } from "../fullscreen";
import { fullscreenEnter, fullscreenExit, icon } from "../icons";
import { controlButton } from "./button";

export function fullscreenButton(parent: Effect, player: HTMLElement, watch: MoqWatch): HTMLElement {
	const button = controlButton(fullscreenEnter, "Fullscreen");

	// The MSE backend renders into a <video>; the WebCodecs backend into a <canvas>.
	const media = () => (watch.querySelector("video") ?? watch.querySelector("canvas")) as HTMLElement | undefined;
	const fullscreen = createFullscreen(player, media);

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

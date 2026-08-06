import type { Effect } from "@moq/signals";
import type MoqWatch from "../../element";
import { captions as captionsIcon } from "../icons";
import { controlButton } from "./button";

/**
 * CC button toggling captions on and off.
 *
 * Hidden entirely when the broadcast publishes no text renditions, so a caption-less stream gets no
 * dead control. Toggling on selects the first rendition; the Captions tab in the settings panel is
 * where a viewer picks between several.
 */
export function captionsButton(parent: Effect, watch: MoqWatch): HTMLElement {
	const button = controlButton(captionsIcon, "Captions");

	parent.run((effect) => {
		const available = Object.keys(effect.get(watch.text.out.available));
		const selected = effect.get(watch.controls.captions);
		const on = selected !== undefined;

		// `.control` sets `display: flex`, which beats the UA rule for the `hidden` attribute, so
		// drop it out of layout the same way the live badge does.
		button.style.display = available.length === 0 ? "none" : "";
		button.classList.toggle("control--active", on);
		button.title = on ? "Turn off captions" : "Turn on captions";
		button.setAttribute("aria-label", button.title);
		button.setAttribute("aria-pressed", String(on));
	});

	parent.event(button, "click", () => {
		// Toggling on picks the first rendition; the settings tab handles choosing among several.
		const first = Object.keys(watch.text.out.available.peek()).at(0);
		watch.captions = watch.controls.captions.peek() !== undefined ? undefined : first;
	});

	return button;
}

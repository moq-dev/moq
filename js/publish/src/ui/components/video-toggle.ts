import type { Effect } from "@moq/signals";
import type MoqPublish from "../../element";
import { camera, cameraOff, icon } from "../icons";
import { controlButton } from "./button";

/** Toggles whether video is published (publish.invisible). Red when off. */
export function videoToggle(parent: Effect, publish: MoqPublish): HTMLElement {
	const button = controlButton(camera, "Hide video");

	parent.run((effect) => {
		const hasSource = effect.get(publish.controls.source) !== undefined;
		const invisible = effect.get(publish.controls.invisible);
		button.disabled = !hasSource;
		button.classList.toggle("control--off", hasSource && invisible);
		button.title = invisible ? "Show video" : "Hide video";
		button.setAttribute("aria-label", button.title);
		button.replaceChildren(icon(invisible ? cameraOff : camera));
	});

	parent.event(button, "click", () => {
		publish.invisible = !publish.invisible;
	});

	return button;
}

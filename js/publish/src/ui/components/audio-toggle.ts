import type { Effect } from "@moq/signals";
import type MoqPublish from "../../element";
import { icon, micOff, microphone } from "../icons";
import { controlButton } from "./button";

/** Toggles publish.muted: stops audio capture and publishes silence in its place. Red when muted. */
export function audioToggle(parent: Effect, publish: MoqPublish): HTMLElement {
	const button = controlButton(microphone, "Mute");

	parent.run((effect) => {
		const hasSource = effect.get(publish.state.source) !== undefined;
		const muted = effect.get(publish.state.muted);
		button.disabled = !hasSource;
		button.classList.toggle("control--off", hasSource && muted);
		button.title = muted ? "Unmute audio" : "Mute audio";
		button.setAttribute("aria-label", button.title);
		button.replaceChildren(icon(muted ? micOff : microphone));
	});

	parent.event(button, "click", () => {
		publish.muted = !publish.muted;
	});

	return button;
}

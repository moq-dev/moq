import type { Effect } from "@moq/signals";
import type MoqPublish from "../../element";
import { icon, micOff, microphone } from "../icons";
import { controlButton } from "./button";

/** Toggles whether audio is published (publish.muted). Red when muted. */
export function audioToggle(parent: Effect, publish: MoqPublish): HTMLElement {
	const button = controlButton(microphone, "Mute");

	parent.run((effect) => {
		const hasSource = effect.get(publish.controls.source) !== undefined;
		const muted = effect.get(publish.controls.muted);
		button.disabled = !hasSource;
		button.classList.toggle("control--off", hasSource && muted);
		button.title = muted ? "Unmute microphone" : "Mute microphone";
		button.setAttribute("aria-label", button.title);
		button.replaceChildren(icon(muted ? micOff : microphone));
	});

	parent.event(button, "click", () => {
		publish.muted = !publish.muted;
	});

	return button;
}

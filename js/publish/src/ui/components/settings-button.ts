import type { Effect } from "@moq/signals";
import { settings } from "../icons";
import type { UiState } from "../state";
import { controlButton } from "./button";

/** Gear button that toggles the settings panel open/closed. */
export function settingsButton(parent: Effect, state: UiState): HTMLElement {
	const button = controlButton(settings, "Settings");

	parent.run((effect) => {
		const open = effect.get(state.panel);
		button.classList.toggle("control--active", open);
		button.title = open ? "Close settings" : "Settings";
		button.setAttribute("aria-label", button.title);
		button.setAttribute("aria-expanded", String(open));
	});

	parent.event(button, "click", () => {
		state.panel.update((v) => !v);
	});

	return button;
}

import type { Effect, Signal } from "@moq/signals";
import settings from "./icons/settings.svg?raw";

/** Wrap a raw SVG string in a decorative, centered span. */
export function icon(svg: string): HTMLElement {
	const span = document.createElement("span");
	span.className = "flex-center";
	span.setAttribute("role", "img");
	span.setAttribute("aria-hidden", "true");
	span.innerHTML = svg;
	return span;
}

/** A round overlay control button with an SVG glyph. */
export function controlButton(svg: string, label: string): HTMLButtonElement {
	const button = document.createElement("button");
	button.type = "button";
	button.className = "control flex-center";
	button.title = label;
	button.setAttribute("aria-label", label);
	button.replaceChildren(icon(svg));
	return button;
}

/** Gear button that toggles the settings panel open/closed. */
export function settingsButton(parent: Effect, panel: Signal<boolean>): HTMLButtonElement {
	const button = controlButton(settings, "Settings");

	parent.run((effect) => {
		const open = effect.get(panel);
		button.classList.toggle("control--active", open);
		button.title = open ? "Close settings" : "Settings";
		button.setAttribute("aria-label", button.title);
		button.setAttribute("aria-expanded", String(open));
	});

	parent.event(button, "click", () => {
		panel.update((v) => !v);
	});

	return button;
}

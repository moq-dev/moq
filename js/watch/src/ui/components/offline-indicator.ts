import type { Effect } from "@moq/signals";
import type MoqWatch from "../../element";

export function offlineIndicator(parent: Effect, watch: MoqWatch): HTMLElement {
	const container = document.createElement("div");
	container.className = "watch-ui__offline-indicator";
	container.setAttribute("role", "status");
	container.setAttribute("aria-live", "polite");

	const text = document.createElement("span");
	text.className = "watch-ui__offline-text";
	text.textContent = "OFFLINE";
	container.appendChild(text);

	parent.run((effect) => {
		const offline = effect.get(watch.broadcast.status) === "offline";
		container.style.display = offline ? "" : "none";
	});

	return container;
}

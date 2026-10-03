import type { Effect } from "@moq/signals";
import type MoqWatch from "../../element";

/** Shows why the origin refused the broadcast, in place of the offline notice. */
export function refusedIndicator(parent: Effect, watch: MoqWatch): HTMLElement {
	const container = document.createElement("div");
	container.className = "watch-ui__notice watch-ui__refused-indicator";
	container.setAttribute("role", "status");
	container.setAttribute("aria-live", "polite");

	const text = document.createElement("span");
	text.className = "watch-ui__notice-text";
	container.appendChild(text);

	parent.run((effect) => {
		const error = effect.get(watch.broadcast.out.error);
		container.style.display = error ? "" : "none";
		if (!error) return;

		text.textContent = error.message
			? `This broadcast was refused: ${error.message}`
			: "This broadcast was refused";
	});

	return container;
}

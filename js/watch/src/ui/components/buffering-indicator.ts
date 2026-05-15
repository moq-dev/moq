import type { Effect } from "@moq/signals";
import type MoqWatch from "../../element";

export function bufferingIndicator(parent: Effect, watch: MoqWatch): HTMLElement {
	const container = document.createElement("div");
	container.className = "watch-ui__buffering flex--center";
	const spinner = document.createElement("div");
	spinner.className = "watch-ui__buffering-spinner";
	container.appendChild(spinner);

	parent.run((effect) => {
		const buffering = effect.get(watch.backend.video.stalled);
		container.style.display = buffering ? "" : "none";
	});

	return container;
}

import type { Effect } from "@moq/signals";
import type MoqWatch from "../../element";

export function bufferingIndicator(parent: Effect, watch: MoqWatch): HTMLElement {
	const container = document.createElement("div");
	container.className = "buffering";
	const spinner = document.createElement("div");
	spinner.className = "buffering-spinner";
	container.appendChild(spinner);

	parent.run((effect) => {
		const buffering = effect.get(watch.backend.video.stalled);
		const offline = effect.get(watch.broadcast.status) === "offline";
		const unsupported = effect.get(watch.backend.video.source.error) === "unsupported";
		container.style.display = buffering && !offline && !unsupported ? "" : "none";
	});

	return container;
}

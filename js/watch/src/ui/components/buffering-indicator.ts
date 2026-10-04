import type { Effect } from "@moq/signals";
import type MoqWatch from "../../element";

export function bufferingIndicator(parent: Effect, watch: MoqWatch): HTMLElement {
	const container = document.createElement("div");
	container.className = "buffering";
	const spinner = document.createElement("div");
	spinner.className = "buffering-spinner";
	container.appendChild(spinner);

	parent.run((effect) => {
		const buffering = effect.get(watch.video.out.stalled);
		const status = effect.get(watch.broadcast.out.status);
		const online = status === "loading" || status === "live";
		const unsupported = effect.get(watch.video.source.out.error) === "unsupported";
		container.style.display = buffering && online && !unsupported ? "" : "none";
	});

	return container;
}

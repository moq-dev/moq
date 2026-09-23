import type { Effect } from "@moq/signals";
import type MoqWatch from "../../element";

export function bufferingIndicator(parent: Effect, watch: MoqWatch): HTMLElement {
	const container = document.createElement("div");
	container.className = "buffering";
	const spinner = document.createElement("div");
	spinner.className = "buffering-spinner";
	container.appendChild(spinner);

	parent.run((effect) => {
		// Audio re-buffers on its own once the ring runs dry, which is just as much a stall as a
		// video one. Gate it on audio actually being downloaded: the ring reports stalled from
		// construction and drains once the download stops, so a video-only broadcast or a muted
		// player would otherwise show the spinner forever.
		const audio = effect.get(watch.audio.in.enabled) && effect.get(watch.audio.source.out.config) !== undefined;
		const buffering = effect.get(watch.video.out.stalled) || (audio && effect.get(watch.audio.out.stalled));
		const offline = effect.get(watch.broadcast.out.status) === "offline";
		const unsupported = effect.get(watch.video.source.out.error) === "unsupported";
		container.style.display = buffering && !offline && !unsupported ? "" : "none";
	});

	return container;
}

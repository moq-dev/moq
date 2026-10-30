import type { Effect } from "@moq/signals";
import type MoqWatch from "../../element";

/** Shows when the player needs an update, or cannot decode the video or, lacking video, the audio. */
export function unsupportedIndicator(parent: Effect, watch: MoqWatch): HTMLElement {
	const container = document.createElement("div");
	container.className = "watch-ui__notice watch-ui__unsupported-indicator";
	container.setAttribute("role", "status");
	container.setAttribute("aria-live", "polite");

	const text = document.createElement("span");
	text.className = "watch-ui__notice-text";
	container.appendChild(text);

	parent.run((effect) => {
		const status = effect.get(watch.broadcast.out.status);
		if (status === "unsupported") {
			container.style.display = "";
			text.textContent = "Update your player to watch this broadcast";
			return;
		}

		// Audio speaks for the broadcast only when there is no video to report on.
		const video = effect.get(watch.video.source.out.catalog);
		const kind = video ? "video" : "audio";
		const source = video ? watch.video.source : watch.audio.source;
		const unsupported = effect.get(source.out.error)?.name === "NotSupportedError";
		const show = unsupported && (status === "loading" || status === "live");
		container.style.display = show ? "" : "none";
		if (!show) return;

		const renditions = (video ?? effect.get(watch.audio.source.out.catalog))?.renditions ?? {};
		const codecs = [...new Set(Object.values(renditions).map((rendition) => rendition.codec))].join(", ");
		text.textContent = codecs
			? `This ${kind} codec is not supported by your browser: ${codecs}`
			: `This ${kind} codec is not supported by your browser`;
	});

	return container;
}

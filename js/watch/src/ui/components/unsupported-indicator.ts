import type { Effect } from "@moq/signals";
import type MoqWatch from "../../element";

/** Shows why video cannot start when every catalog rendition is unsupported. */
export function unsupportedIndicator(parent: Effect, watch: MoqWatch): HTMLElement {
	const container = document.createElement("div");
	container.className = "watch-ui__unsupported-indicator";
	container.setAttribute("role", "status");
	container.setAttribute("aria-live", "polite");

	const text = document.createElement("span");
	text.className = "watch-ui__unsupported-text";
	container.appendChild(text);

	parent.run((effect) => {
		const unsupported = effect.get(watch.backend.video.source.error) === "unsupported";
		const offline = effect.get(watch.broadcast.status) === "offline";
		const show = unsupported && !offline;
		container.style.display = show ? "" : "none";
		if (!show) return;

		const renditions = effect.get(watch.backend.video.source.catalog)?.renditions ?? {};
		const codecs = [...new Set(Object.values(renditions).map((rendition) => rendition.codec))].join(", ");
		text.textContent = codecs
			? `This video codec is not supported by your browser: ${codecs}`
			: "This video codec is not supported by your browser";
	});

	return container;
}

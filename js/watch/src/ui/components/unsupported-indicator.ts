import type { Effect } from "@moq/signals";
import type MoqWatch from "../../element";

/**
 * Shows a notice when the catalog has video renditions but this browser/hardware can decode none of
 * them, so the viewer sees the cause instead of an indefinite buffering spinner. Codec support is
 * hardware-dependent, so this can appear on one machine and not another for the same broadcast.
 */
export function unsupportedIndicator(parent: Effect, watch: MoqWatch): HTMLElement {
	const container = document.createElement("div");
	container.className = "watch-ui__unsupported-indicator";
	container.setAttribute("role", "status");
	container.setAttribute("aria-live", "polite");

	const text = document.createElement("span");
	text.className = "watch-ui__unsupported-text";
	container.appendChild(text);

	parent.run((effect) => {
		const unsupported = effect.get(watch.backend.video.source.unsupported);
		const offline = effect.get(watch.broadcast.status) === "offline";
		const show = unsupported && !offline;
		container.style.display = show ? "" : "none";
		if (!show) return;

		// Name the codec(s) so the cause is obvious (e.g. an HEVC-only publisher seen from a browser
		// without HEVC decode).
		const renditions = effect.get(watch.backend.video.source.catalog)?.renditions ?? {};
		const codecs = [...new Set(Object.values(renditions).map((r) => r.codec))].join(", ");
		text.textContent = codecs
			? `This video codec isn't supported by your browser: ${codecs}`
			: "This video codec isn't supported by your browser";
	});

	return container;
}

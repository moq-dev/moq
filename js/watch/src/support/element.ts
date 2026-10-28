/**
 * The `<moq-watch-support>` custom element: renders the {@link isSupported} report.
 *
 * Side-effectful: importing this registers the element.
 *
 * @module
 */

import { type Level, type Report, Support } from "@moq/ui";
import { type Full, isSupported } from "./";

export type { Show } from "@moq/ui";

function summarize(support: Full): Level {
	if (support.webtransport === "none") return "none";

	if (!support.audio.decoding || !support.video.decoding) return "none";
	if (!support.audio.render || !support.video.render) return "none";

	if (!Object.values(support.audio.decoding).some((v) => v !== "none")) return "none";
	if (!Object.values(support.video.decoding).some((v) => v.software || v.hardware)) return "none";

	if (!Object.values(support.audio.decoding).every((v) => v === "full")) return "partial";
	if (!Object.values(support.video.decoding).every((v) => v.software || v.hardware)) return "partial";

	return "full";
}

/** The `<moq-watch-support>` element: a banner reporting what this browser can play. */
export default class MoqWatchSupport extends Support {
	/** Detect what this browser can play. */
	protected async detect(): Promise<Report> {
		const support = await isSupported();
		return {
			summary: summarize(support),
			rows: [
				["WebTransport", "", support.webtransport],
				["Rendering", "Audio", support.audio.render],
				["", "Video", support.video.render],
				["Decoding", "Opus", support.audio.decoding.opus],
				["", "AAC", support.audio.decoding.aac],
				["", "AV1", support.video.decoding?.av1],
				["", "H.265", support.video.decoding?.h265],
				["", "H.264", support.video.decoding?.h264],
				["", "VP9", support.video.decoding?.vp9],
				["", "VP8", support.video.decoding?.vp8],
			],
		};
	}
}

customElements.define("moq-watch-support", MoqWatchSupport);

declare global {
	interface HTMLElementTagNameMap {
		"moq-watch-support": MoqWatchSupport;
	}
}

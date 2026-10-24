/**
 * The `<moq-publish-support>` custom element: renders the {@link isSupported} report.
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

	if (!support.audio.encoding || !support.video.encoding) return "none";
	if (!support.audio.capture) return "none";

	if (!Object.values(support.audio.encoding).some((v) => v !== "none")) return "none";
	if (!Object.values(support.video.encoding).some((v) => v.software || v.hardware)) return "none";

	if (support.video.capture === "partial") return "partial";

	if (!Object.values(support.video.encoding).some((v) => v.hardware)) return "partial";

	return "full";
}

/** The `<moq-publish-support>` element: a banner reporting what this browser can publish. */
export default class MoqPublishSupport extends Support {
	protected async detect(): Promise<Report> {
		const support = await isSupported();
		return {
			summary: summarize(support),
			rows: [
				["WebTransport", "", support.webtransport],
				["Capture", "Audio", support.audio.capture],
				["", "Video", support.video.capture],
				["Encoding", "Opus", support.audio.encoding.opus],
				["", "AAC", support.audio.encoding.aac],
				["", "AV1", support.video.encoding?.av1],
				["", "H.265", support.video.encoding?.h265],
				["", "H.264", support.video.encoding?.h264],
				["", "VP9", support.video.encoding?.vp9],
				["", "VP8", support.video.encoding?.vp8],
			],
		};
	}
}

customElements.define("moq-publish-support", MoqPublishSupport);

declare global {
	interface HTMLElementTagNameMap {
		"moq-publish-support": MoqPublishSupport;
	}
}

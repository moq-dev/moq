import type { Effect } from "@moq/signals";
import type MoqPublish from "../../element";

type Variant = "live" | "audio-only" | "video-only" | "warning" | "connecting" | "error";

function deriveStatus(
	url: URL | undefined,
	status: "connecting" | "connected" | "disconnected",
	hasAudio: boolean,
	hasVideo: boolean,
): { variant: Variant; text: string } {
	if (!url) return { variant: "error", text: "No URL" };
	if (status === "disconnected") return { variant: "error", text: "Disconnected" };
	if (status === "connecting") return { variant: "connecting", text: "Connecting" };
	if (!hasAudio && !hasVideo) return { variant: "warning", text: "No source" };
	if (!hasAudio && hasVideo) return { variant: "video-only", text: "Video only" };
	if (hasAudio && !hasVideo) return { variant: "audio-only", text: "Audio only" };
	return { variant: "live", text: "Live" };
}

/** Publishing status pill: Live / Audio only / Video only / Connecting / etc. */
export function statusBadge(parent: Effect, publish: MoqPublish): HTMLElement {
	const wrapper = document.createElement("div");
	wrapper.className = "badge";

	const dot = document.createElement("span");
	dot.className = "badge-dot";
	const text = document.createElement("span");
	text.className = "badge-text";
	wrapper.append(dot, text);

	parent.run((effect) => {
		const url = effect.get(publish.connection.url);
		const status = effect.get(publish.connection.status);
		const audioSource = effect.get(publish.audio.in.source);
		const videoSource = effect.get(publish.capture.in.source);
		const muted = effect.get(publish.controls.muted);
		const invisible = effect.get(publish.controls.invisible);

		const { variant, text: label } = deriveStatus(
			url,
			status,
			!!audioSource && !muted,
			!!videoSource && !invisible,
		);
		wrapper.dataset.variant = variant;
		text.textContent = label.toUpperCase();
	});

	return wrapper;
}

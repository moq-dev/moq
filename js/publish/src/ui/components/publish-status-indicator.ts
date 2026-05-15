import type { Effect } from "@moq/signals";
import type MoqPublish from "../../element";

type StatusConfig = { variant: string; text: string };

function deriveStatus(
	url: URL | undefined,
	status: "connecting" | "connected" | "disconnected",
	hasAudio: boolean,
	hasVideo: boolean,
): StatusConfig {
	if (!url) return { variant: "error", text: "No URL" };
	if (status === "disconnected") return { variant: "error", text: "Disconnected" };
	if (status === "connecting") return { variant: "connecting", text: "Connecting..." };
	if (!hasAudio && !hasVideo) return { variant: "warning", text: "Select Source" };
	if (!hasAudio && hasVideo) return { variant: "video-only", text: "Video Only" };
	if (hasAudio && !hasVideo) return { variant: "audio-only", text: "Audio Only" };
	return { variant: "live", text: "Live" };
}

export function publishStatusIndicator(parent: Effect, publish: MoqPublish): HTMLElement {
	const wrapper = document.createElement("div");
	wrapper.className = "publish-ui__status-indicator flex--center";

	const dot = document.createElement("span");
	const text = document.createElement("span");
	wrapper.append(dot, text);

	parent.run((effect) => {
		const url = effect.get(publish.connection.url);
		const status = effect.get(publish.connection.status);
		const audioSource = effect.get(publish.broadcast.audio.source);
		const videoSource = effect.get(publish.broadcast.video.source);
		const muted = effect.get(publish.state.muted);
		const invisible = effect.get(publish.state.invisible);

		const { variant, text: label } = deriveStatus(
			url,
			status,
			!!audioSource && !muted,
			!!videoSource && !invisible,
		);
		dot.className = `publish-ui__status-indicator-dot publish-ui__status-indicator-dot--${variant}`;
		text.className = `publish-ui__status-indicator-text publish-ui__status-indicator-text--${variant}`;
		text.textContent = label;
	});

	return wrapper;
}

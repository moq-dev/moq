import type { Effect } from "@moq/signals";
import type MoqPublish from "../../element";
import { Camera, Microphone } from "../../source";

type Variant = "live" | "audio-only" | "video-only" | "warning" | "connecting" | "error";

function deriveStatus(
	url: URL | undefined,
	status: "connecting" | "connected" | "disconnected",
	hasAudio: boolean,
	hasVideo: boolean,
	videoError: Error | undefined,
	audioError: Error | undefined,
): { variant: Variant; text: string } {
	if (videoError || audioError) {
		const input = videoError && audioError ? "Camera and microphone" : videoError ? "Camera" : "Microphone";
		const denied = [videoError, audioError].filter(Boolean).every((error) => error?.name === "NotAllowedError");
		return { variant: "error", text: `${input} ${denied ? "denied" : "failed"}` };
	}
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
		const audioCapture = effect.get(publish.audio.in.capture);
		const audioSource = audioCapture ? effect.get(audioCapture.in.source) : undefined;
		const videoCapture = effect.get(publish.video.in.capture);
		const videoSource = videoCapture ? effect.get(videoCapture.in.source) : undefined;
		const muted = effect.get(publish.controls.muted);
		const invisible = effect.get(publish.controls.invisible);
		const video = effect.get(publish.sources.video);
		const audio = effect.get(publish.sources.audio);
		const videoError = video instanceof Camera ? effect.get(video.out.error) : undefined;
		const audioError = audio instanceof Microphone ? effect.get(audio.out.error) : undefined;

		const { variant, text: label } = deriveStatus(
			url,
			status,
			!!audioSource && !muted,
			!!videoSource && !invisible,
			videoError,
			audioError,
		);
		wrapper.dataset.variant = variant;
		text.textContent = label.toUpperCase();
	});

	return wrapper;
}

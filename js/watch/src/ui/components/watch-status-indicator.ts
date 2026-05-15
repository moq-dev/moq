import type { Effect } from "@moq/signals";
import type MoqWatch from "../../element";

type StatusConfig = { variant: string; text: string };

function deriveStatus(
	url: URL | undefined,
	connection: "connecting" | "connected" | "disconnected",
	broadcast: "offline" | "loading" | "live",
): StatusConfig {
	if (!url) return { variant: "error", text: "No URL" };
	if (connection === "disconnected") return { variant: "error", text: "Disconnected" };
	if (connection === "connecting") return { variant: "connecting", text: "Connecting..." };
	if (broadcast === "offline") return { variant: "error", text: "Offline" };
	if (broadcast === "loading") return { variant: "loading", text: "Loading..." };
	if (broadcast === "live") return { variant: "live", text: "Live" };
	return { variant: "connected", text: "Connected" };
}

export function watchStatusIndicator(parent: Effect, watch: MoqWatch): HTMLElement {
	const wrapper = document.createElement("div");
	wrapper.className = "watch-ui__status-indicator flex--center";

	const dot = document.createElement("span");
	const text = document.createElement("span");
	wrapper.append(dot, text);

	parent.run((effect) => {
		const url = effect.get(watch.connection.url);
		const conn = effect.get(watch.connection.status);
		const broadcast = effect.get(watch.broadcast.status);
		const { variant, text: label } = deriveStatus(url, conn, broadcast);

		dot.className = `watch-ui__status-indicator-dot watch-ui__status-indicator-dot--${variant}`;
		text.className = `watch-ui__status-indicator-text watch-ui__status-indicator-text--${variant}`;
		text.textContent = label;
	});

	return wrapper;
}

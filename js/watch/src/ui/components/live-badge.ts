import type { Effect } from "@moq/signals";
import type MoqWatch from "../../element";
import { formatMillis } from "../format";
import type { UiState } from "../state";

type Variant = "live" | "loading" | "connecting" | "error";

function deriveStatus(
	url: URL | undefined,
	connection: "connecting" | "connected" | "disconnected",
	broadcast: "offline" | "loading" | "live",
): { variant: Variant; text: string } {
	if (!url) return { variant: "error", text: "No URL" };
	if (connection === "disconnected") return { variant: "error", text: "Disconnected" };
	if (connection === "connecting") return { variant: "connecting", text: "Connecting" };
	if (broadcast === "offline") return { variant: "error", text: "Offline" };
	if (broadcast === "loading") return { variant: "loading", text: "Loading" };
	if (broadcast === "live") return { variant: "live", text: "Live" };
	return { variant: "loading", text: "Connected" };
}

/**
 * Compact status pill. Shows a pulsing dot + LIVE when streaming, plus the
 * current buffer latency. Clicking jumps to the Latency tab.
 */
export function liveBadge(parent: Effect, watch: MoqWatch, state: UiState): HTMLElement {
	const button = document.createElement("button");
	button.type = "button";
	button.className = "badge flex-align-center";
	button.title = "Latency settings";

	const dot = document.createElement("span");
	dot.className = "badge-dot";
	const text = document.createElement("span");
	text.className = "badge-text";
	const latency = document.createElement("span");
	latency.className = "badge-latency";

	button.append(dot, text, latency);

	parent.run((effect) => {
		const url = effect.get(watch.connection.url);
		const conn = effect.get(watch.connection.status);
		const broadcast = effect.get(watch.broadcast.status);
		const { variant, text: label } = deriveStatus(url, conn, broadcast);

		button.dataset.variant = variant;
		text.textContent = label.toUpperCase();
	});

	parent.run((effect) => {
		const mode = effect.get(watch.backend.latency);
		const jitter = effect.get(watch.backend.jitter);
		latency.textContent = mode === "real-time" ? `auto ${formatMillis(jitter)}` : formatMillis(jitter);
	});

	parent.event(button, "click", () => {
		state.tab.set("latency");
		state.panel.set(true);
	});

	return button;
}

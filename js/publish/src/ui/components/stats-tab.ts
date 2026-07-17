import type { Effect, Getter } from "@moq/signals";
import * as DOM from "@moq/signals/dom";
import type MoqPublish from "../../element";
import { formatBitrate, formatFps, formatHz } from "../format";
import { graph } from "../graph";
import { audio as audioIcon, icon, video as videoIcon, wifi as wifiIcon } from "../icons";

const POLL_MS = 250;

type Kind = "network" | "video" | "audio";

function card(kind: Kind, label: string, svg: string): { el: HTMLElement; grid: HTMLElement; status: HTMLElement } {
	const el = DOM.create("div", { className: `stat-card stat-card--${kind}` });
	const head = DOM.create("div", { className: "stat-head" });
	const iconWrap = DOM.create("div", { className: "stat-icon" });
	iconWrap.appendChild(icon(svg));
	const status = DOM.create("span", { className: "stat-status", style: { display: "none" } });
	head.append(iconWrap, DOM.create("span", { className: "stat-title" }, label), status);
	const grid = DOM.create("div", { className: "stat-grid" });
	el.append(head, grid);
	return { el, grid, status };
}

/** Show an active/idle pill: an encoder only runs (and uses bandwidth) while a viewer is subscribed. */
function trackActive(parent: Effect, status: HTMLElement, active: Getter<boolean | undefined>) {
	parent.run((effect) => {
		const on = effect.get(active);
		status.style.display = "";
		status.textContent = on ? "active" : "idle";
		status.className = `stat-status stat-status--${on ? "active" : "idle"}`;
	});
}

function line(grid: HTMLElement, label: string): HTMLSpanElement {
	const row = DOM.create("div", { className: "stat-line" });
	const value = DOM.create("span", { className: "stat-value" }, "—");
	row.append(DOM.create("span", { className: "stat-key" }, label), value);
	grid.appendChild(row);
	return value;
}

/** The Stats tab: what we're capturing and publishing. */
export function statsTab(parent: Effect, publish: MoqPublish): HTMLElement {
	const container = DOM.create("div", { className: "tab-body" });

	// Video card: static detail as rows, live capture fps + upload bitrate as graphs.
	const videoCard = card("video", "Video", videoIcon);
	const vRes = line(videoCard.grid, "Resolution");
	const vCodec = line(videoCard.grid, "Codec");
	const vBitrateGraph = graph(parent, "Bitrate", { color: "#a855f7", format: formatBitrate });
	const vFpsGraph = graph(parent, "Frame rate", { color: "#facc15", format: formatFps });
	videoCard.el.append(vBitrateGraph.el, vFpsGraph.el);

	// active = a viewer is subscribed and we're actually encoding/sending.
	trackActive(parent, videoCard.status, publish.video.out.active);

	const audioCard = card("audio", "Audio", audioIcon);
	const aCodec = line(audioCard.grid, "Codec");
	const aRate = line(audioCard.grid, "Sample rate");
	const aChannels = line(audioCard.grid, "Channels");
	const aBitrate = line(audioCard.grid, "Bitrate");
	trackActive(parent, audioCard.status, publish.audio.out.active);

	const netCard = card("network", "Connection", wifiIcon);
	const nStatus = line(netCard.grid, "Status");
	const nServer = line(netCard.grid, "Server");
	const nName = line(netCard.grid, "Broadcast");

	container.append(videoCard.el, audioCard.el, netCard.el);

	// Resolution/codec from the live capture (display) + catalog; card hides when not capturing video.
	parent.run((effect) => {
		const display = effect.get(publish.capture.out.display);
		const cfg = effect.get(publish.video.out.catalog);
		videoCard.el.style.display = display ? "" : "none";
		vRes.textContent = display ? `${display.width}×${display.height}` : "—";
		vCodec.textContent = cfg?.codec ?? "—";
	});

	parent.run((effect) => {
		const cfg = effect.get(publish.audio.out.catalog);
		audioCard.el.style.display = cfg ? "" : "none";
		if (!cfg) return;
		aCodec.textContent = cfg.codec ?? "—";
		aRate.textContent = cfg.sampleRate ? formatHz(cfg.sampleRate) : "—";
		aChannels.textContent = cfg.numberOfChannels ? `${cfg.numberOfChannels}` : "—";
		aBitrate.textContent = cfg.bitrate ? formatBitrate(cfg.bitrate) : "—";
	});

	parent.run((effect) => {
		const url = effect.get(publish.connection.url);
		const status = effect.get(publish.connection.status);
		const name = effect.get(publish.broadcast.in.name);
		nStatus.textContent = status;
		nServer.textContent = url?.host ?? "—";
		nName.textContent = name?.toString() || "—";
	});

	// Live graphs: frame rate from captured frames, bitrate measured from the bytes we encoded.
	// The congestion controller's send estimate would be simpler, but Safari's WebTransport has no
	// getStats(), so it has no estimate at all and the graph stayed empty there.
	let frames = 0;
	parent.subscribe(publish.capture.out.frame, () => {
		frames++;
	});

	const encoded = () => publish.video.out.stats.peek().bytes;

	let prevFrames = 0;
	let prevBytes = encoded();
	let prevWhen = performance.now();

	parent.interval(() => {
		const now = performance.now();
		const elapsed = now - prevWhen;
		prevWhen = now;

		const frameDelta = frames - prevFrames;
		prevFrames = frames;
		vFpsGraph.push(elapsed > 0 && frameDelta > 0 ? frameDelta / (elapsed / 1000) : undefined);

		const bytes = encoded();
		const byteDelta = bytes - prevBytes;
		prevBytes = bytes;
		vBitrateGraph.push(elapsed > 0 && byteDelta > 0 ? (byteDelta * 8) / (elapsed / 1000) : undefined);
	}, POLL_MS);

	return container;
}

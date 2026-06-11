import type { Catalog } from "@moq/hang";
import type { Effect } from "@moq/signals";
import * as DOM from "@moq/signals/dom";
import type MoqPublish from "../../element";
import { formatBitrate, formatHz } from "../format";
import { audio as audioIcon, icon, video as videoIcon, wifi as wifiIcon } from "../icons";

type Kind = "network" | "video" | "audio";

function card(kind: Kind, label: string, svg: string): { el: HTMLElement; grid: HTMLElement } {
	const el = DOM.create("div", { className: `stat-card stat-card--${kind}` });
	const head = DOM.create("div", { className: "stat-head" });
	const iconWrap = DOM.create("div", { className: "stat-icon" });
	iconWrap.appendChild(icon(svg));
	head.append(iconWrap, DOM.create("span", { className: "stat-title" }, label));
	const grid = DOM.create("div", { className: "stat-grid" });
	el.append(head, grid);
	return { el, grid };
}

function line(grid: HTMLElement, label: string): HTMLSpanElement {
	const row = DOM.create("div", { className: "stat-line" });
	const value = DOM.create("span", { className: "stat-value" }, "—");
	row.append(DOM.create("span", { className: "stat-key" }, label), value);
	grid.appendChild(row);
	return value;
}

function firstRendition<T>(catalog: { renditions?: Record<string, T> } | undefined): T | undefined {
	return catalog ? Object.values(catalog.renditions ?? {})[0] : undefined;
}

/** The Stats tab: what we're currently publishing. */
export function statsTab(parent: Effect, publish: MoqPublish): HTMLElement {
	const container = DOM.create("div", { className: "tab-body" });

	const videoCard = card("video", "Video", videoIcon);
	const vRes = line(videoCard.grid, "Resolution");
	const vCodec = line(videoCard.grid, "Codec");
	const vFps = line(videoCard.grid, "Frame rate");
	const vRate = line(videoCard.grid, "Bitrate");

	const audioCard = card("audio", "Audio", audioIcon);
	const aCodec = line(audioCard.grid, "Codec");
	const aRate = line(audioCard.grid, "Sample rate");
	const aChannels = line(audioCard.grid, "Channels");
	const aBitrate = line(audioCard.grid, "Bitrate");

	const netCard = card("network", "Connection", wifiIcon);
	const nStatus = line(netCard.grid, "Status");
	const nServer = line(netCard.grid, "Server");
	const nName = line(netCard.grid, "Broadcast");

	container.append(videoCard.el, audioCard.el, netCard.el);

	parent.run((effect) => {
		const catalog = effect.get(publish.broadcast.video.catalog) as Catalog.Video | undefined;
		const cfg = firstRendition<Catalog.VideoConfig>(catalog);
		const display = catalog?.display;
		const present = !!cfg;
		videoCard.el.style.display = present ? "" : "none";
		if (!cfg) return;
		const w = display?.width ?? cfg.codedWidth;
		const h = display?.height ?? cfg.codedHeight;
		vRes.textContent = w && h ? `${w}×${h}` : "—";
		vCodec.textContent = cfg.codec ?? "—";
		vFps.textContent = cfg.framerate ? `${Math.round(cfg.framerate)} fps` : "—";
		vRate.textContent = cfg.bitrate ? formatBitrate(cfg.bitrate) : "—";
	});

	parent.run((effect) => {
		const catalog = effect.get(publish.broadcast.audio.catalog) as Catalog.Audio | undefined;
		const cfg = firstRendition<Catalog.AudioConfig>(catalog);
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
		const name = effect.get(publish.broadcast.name);
		nStatus.textContent = status;
		nServer.textContent = url?.host ?? "—";
		nName.textContent = name?.toString() || "—";
	});

	return container;
}

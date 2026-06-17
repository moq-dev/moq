import type { Effect, Getter } from "@moq/signals";
import * as DOM from "@moq/signals/dom";
import type MoqWatch from "../element";
import { formatBitrate, formatFps, formatHz, formatMillis } from "./format";
import { graph } from "./graph";
import { audio as audioIcon, icon, network as networkIcon, video as videoIcon } from "./icons";

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

function line(grid: HTMLElement, label: string): HTMLSpanElement {
	const row = DOM.create("div", { className: "stat-line" });
	const value = DOM.create("span", { className: "stat-value" }, "—");
	row.append(DOM.create("span", { className: "stat-key" }, label), value);
	grid.appendChild(row);
	return value;
}

/** Bitrate from a byte counter, sampled across an interval. */
function rate(prev: { bytes: number; when: number }, bytes: number, now: number): number | undefined {
	if (prev.bytes <= 0) return undefined;
	const elapsed = now - prev.when;
	const delta = bytes - prev.bytes;
	if (delta <= 0 || elapsed <= 0) return undefined;
	// bytes → bits (*8); elapsed is ms, so *1000/elapsed gives a per-second rate.
	return delta * 8 * (1000 / elapsed);
}

function hasRenditions(catalog: { renditions?: Record<string, unknown> } | undefined): boolean {
	return Object.keys(catalog?.renditions ?? {}).length > 0;
}

interface TrackOptions {
	// Hide the card entirely when this catalog has no renditions.
	catalog: Getter<{ renditions?: Record<string, unknown> } | undefined>;
	// Show the status pill when this is true (e.g. muted / paused).
	flag: Getter<boolean>;
	label: string;
}

/** Hide a card when its media isn't in the catalog; show a status pill otherwise. */
function track(parent: Effect, card: { el: HTMLElement; status: HTMLElement }, opts: TrackOptions) {
	card.status.textContent = opts.label;
	parent.run((effect) => {
		const present = hasRenditions(effect.get(opts.catalog));
		card.el.style.display = present ? "" : "none";
		card.status.style.display = present && effect.get(opts.flag) ? "" : "none";
	});
}

/** The Stats tab: live codec/network detail plus rolling graphs. */
export function statsTab(parent: Effect, watch: MoqWatch): HTMLElement {
	const container = DOM.create("div", { className: "tab-body stats" });

	// Video card: static detail as rows, live bitrate/fps as graphs (no duplicate rows).
	const videoCard = card("video", "Video", videoIcon);
	const vRes = line(videoCard.grid, "Resolution");
	const vCodec = line(videoCard.grid, "Codec");
	const vBitrateGraph = graph(parent, "Bitrate", { color: "#a855f7", format: formatBitrate });
	const vFpsGraph = graph(parent, "Frame rate", { color: "#facc15", format: formatFps });
	videoCard.el.append(vBitrateGraph.el, vFpsGraph.el);
	track(parent, videoCard, {
		catalog: watch.backend.video.source.catalog,
		flag: watch.backend.paused,
		label: "paused",
	});

	// Audio card.
	const audioCard = card("audio", "Audio", audioIcon);
	const aCodec = line(audioCard.grid, "Codec");
	const aRate2 = line(audioCard.grid, "Sample rate");
	const aChannels = line(audioCard.grid, "Channels");
	const aBitrate = line(audioCard.grid, "Bitrate");
	track(parent, audioCard, {
		catalog: watch.backend.audio.source.catalog,
		flag: watch.backend.audio.muted,
		label: "muted",
	});

	// Network card: congestion-control estimate vs. the bitrate we actually pull.
	const netCard = card("network", "Network", networkIcon);
	const nMax = line(netCard.grid, "Estimated max");
	const nActual = line(netCard.grid, "Actual");
	const nRttGraph = graph(parent, "Round trip", { color: "#00dfff", format: (v) => formatMillis(v) });
	netCard.el.append(nRttGraph.el);

	container.append(videoCard.el, audioCard.el, netCard.el);

	let vPrev = { frames: 0, bytes: 0, when: performance.now() };
	let aPrev = { bytes: 0, when: performance.now() };

	parent.interval(() => {
		const now = performance.now();

		// Video. Resolution comes from the active rendition (catalog.display is optional).
		const vConf = watch.backend.video.source.config.peek();
		const vCat = watch.backend.video.source.catalog.peek();
		const vStats = watch.backend.video.stats.peek();
		const w = vConf?.codedWidth ?? vCat?.display?.width;
		const h = vConf?.codedHeight ?? vCat?.display?.height;
		vRes.textContent = w && h ? `${w}×${h}` : "—";
		vCodec.textContent = vConf?.codec ?? "—";

		let fps: number | undefined;
		if (vStats && vPrev.frames > 0) {
			const elapsed = now - vPrev.when;
			const delta = vStats.frameCount - vPrev.frames;
			if (delta > 0 && elapsed > 0) fps = delta / (elapsed / 1000);
		}
		const vBitrate = vStats ? rate(vPrev, vStats.bytesReceived, now) : undefined;
		vBitrateGraph.push(vBitrate);
		vFpsGraph.push(fps);
		if (vStats) vPrev = { frames: vStats.frameCount, bytes: vStats.bytesReceived, when: now };

		// Audio.
		const aConf = watch.backend.audio.source.config.peek();
		const aStats = watch.backend.audio.stats.peek();
		aCodec.textContent = aConf?.codec ?? "—";
		aRate2.textContent = aConf?.sampleRate ? formatHz(aConf.sampleRate) : "—";
		aChannels.textContent = aConf?.numberOfChannels ? `${aConf.numberOfChannels}` : "—";
		const aBitrate2 = aStats ? rate(aPrev, aStats.bytesReceived, now) : undefined;
		aBitrate.textContent = aBitrate2 !== undefined ? formatBitrate(aBitrate2) : "—";
		if (aStats) aPrev = { bytes: aStats.bytesReceived, when: now };

		// Network. "Estimated max" is the congestion controller / PROBE estimate;
		// "Actual" is the goodput we measure from the video + audio byte counters.
		const conn = watch.connection.established.peek();
		const estimate = conn?.recvBandwidth?.peek();
		nMax.textContent = estimate ? formatBitrate(estimate) : "—";
		const actual =
			vBitrate !== undefined || aBitrate2 !== undefined ? (vBitrate ?? 0) + (aBitrate2 ?? 0) : undefined;
		nActual.textContent = actual !== undefined ? formatBitrate(actual) : "—";
		const rtt = conn?.rtt?.peek();
		nRttGraph.push(rtt !== undefined && rtt > 0 ? rtt : undefined);
	}, POLL_MS);

	return container;
}

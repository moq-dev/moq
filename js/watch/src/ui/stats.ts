import type { Effect, Signal } from "@moq/signals";
import * as DOM from "@moq/signals/dom";
import type MoqWatch from "../element";
import { audio, buffer, icon, network, video } from "./icons";

const POLL_MS = 250;

type Kind = "network" | "video" | "audio" | "buffer";

function row(kind: Kind, label: string, svg: string): { el: HTMLElement; data: HTMLSpanElement } {
	const el = DOM.create("div", { className: `stats-item stats-item--${kind}` });

	const iconWrap = DOM.create("div", { className: "stats-icon-wrapper" });
	iconWrap.appendChild(icon(svg));

	const title = DOM.create("span", { className: "stats-item-title" }, label);
	const data = DOM.create("span", { className: "stats-item-data" }, "N/A");

	const detail = DOM.create("div", { className: "stats-item-detail" });
	detail.append(title, data);

	el.append(iconWrap, detail);
	return { el, data };
}

function formatBitrate(bps: number): string {
	if (bps >= 1_000_000) return `${(bps / 1_000_000).toFixed(1)}Mbps`;
	if (bps >= 1_000) return `${(bps / 1_000).toFixed(0)}kbps`;
	return `${bps.toFixed(0)}bps`;
}

function formatBandwidth(bps: number | undefined, dir: "up" | "down"): string | null {
	if (bps === undefined || bps <= 0) return null;
	const arrow = dir === "down" ? "↓" : "↑";
	if (bps >= 1_000_000_000) return `${arrow} ${(bps / 1_000_000_000).toFixed(1)}Gbps`;
	return `${arrow} ${formatBitrate(bps)}`;
}

function networkRow(parent: Effect, watch: MoqWatch): HTMLElement {
	const { el, data } = row("network", "network", network);

	parent.interval(() => {
		const conn = watch.connection.established.peek();
		if (!conn) {
			data.textContent = "N/A";
			return;
		}
		const rtt = conn.rtt?.peek();
		const parts = [
			formatBandwidth(conn.recvBandwidth?.peek(), "down"),
			formatBandwidth(conn.sendBandwidth?.peek(), "up"),
			rtt !== undefined && rtt > 0 ? `${rtt.toFixed(0)}ms` : null,
		].filter((p): p is string => p !== null);
		data.textContent = parts.length > 0 ? parts.join("\n") : "N/A";
	}, POLL_MS);

	return el;
}

function videoRow(parent: Effect, watch: MoqWatch): HTMLElement {
	const { el, data } = row("video", "video", video);
	let prevFrames = 0;
	let prevBytes = 0;
	let prevWhen = performance.now();

	parent.interval(() => {
		const catalog = watch.backend.video.source.catalog.peek();
		const stats = watch.backend.video.stats.peek();
		const now = performance.now();
		const elapsedMs = now - prevWhen;

		let fps: number | undefined;
		if (stats && prevFrames > 0 && elapsedMs > 0) {
			const delta = stats.frameCount - prevFrames;
			if (delta > 0) fps = delta / (elapsedMs / 1000);
		}

		let bitrate: string | undefined;
		if (stats && prevBytes > 0 && elapsedMs > 0) {
			const delta = stats.bytesReceived - prevBytes;
			if (delta > 0) bitrate = formatBitrate(delta * 8 * (1000 / elapsedMs));
		}

		if (stats) {
			prevFrames = stats.frameCount;
			prevBytes = stats.bytesReceived;
			prevWhen = now;
		}

		const { width, height } = catalog?.display ?? {};
		data.textContent = [
			width && height ? `${width}x${height}` : "N/A",
			fps !== undefined ? `@${fps.toFixed(1)} fps` : "N/A",
			bitrate ?? "N/A",
		].join("\n");
	}, POLL_MS);

	return el;
}

function audioRow(parent: Effect, watch: MoqWatch): HTMLElement {
	const { el, data } = row("audio", "audio", audio);
	let prevBytes = 0;
	let prevWhen = performance.now();

	parent.interval(() => {
		const track = watch.backend.audio.source.track.peek();
		const config = watch.backend.audio.source.config.peek();
		const stats = watch.backend.audio.stats.peek();

		if (!track || !config) {
			data.textContent = "N/A";
			return;
		}

		const now = performance.now();
		let bitrate: string | undefined;
		if (stats && prevBytes > 0) {
			const delta = stats.bytesReceived - prevBytes;
			const elapsedMs = now - prevWhen;
			if (delta > 0 && elapsedMs > 0) bitrate = formatBitrate(delta * 8 * (1000 / elapsedMs));
		}

		if (stats) {
			prevBytes = stats.bytesReceived;
			prevWhen = now;
		}

		const parts: string[] = [];
		if (config.sampleRate) parts.push(`${(config.sampleRate / 1000).toFixed(1)}kHz`);
		if (config.numberOfChannels) parts.push(`${config.numberOfChannels}ch`);
		parts.push(bitrate ?? "N/A");
		if (config.codec) parts.push(config.codec);
		data.textContent = parts.length > 0 ? parts.join("\n") : "N/A";
	}, POLL_MS);

	return el;
}

function bufferRow(parent: Effect, watch: MoqWatch): HTMLElement {
	const { el, data } = row("buffer", "buffer", buffer);

	parent.run((effect) => {
		const jitter = effect.get(watch.backend.jitter);
		data.textContent = `${Math.round(jitter)}ms`;
	});

	return el;
}

export function statsPanel(parent: Effect, watch: MoqWatch, visible: Signal<boolean>): HTMLElement {
	const wrap = DOM.create("div", { className: "stats" });
	const panel = DOM.create("div", { className: "stats-panel" });
	wrap.appendChild(panel);

	parent.run((effect) => {
		const showing = effect.get(visible);
		wrap.style.display = showing ? "" : "none";
		if (!showing) return;

		DOM.render(effect, panel, networkRow(effect, watch));
		DOM.render(effect, panel, videoRow(effect, watch));
		DOM.render(effect, panel, audioRow(effect, watch));
		DOM.render(effect, panel, bufferRow(effect, watch));
	});

	return wrap;
}

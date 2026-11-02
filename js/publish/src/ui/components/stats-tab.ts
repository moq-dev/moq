import { Milli, Nano, now } from "@moq/clock";
import type { Effect, Getter } from "@moq/signals";
import * as DOM from "@moq/signals/dom";
import { formatBitrate, formatFps, formatHz, graph, statCard, statLine } from "@moq/ui";
import type MoqPublish from "../../element";
import { audio as audioIcon, video as videoIcon, wifi as wifiIcon } from "../icons";

const POLL = Nano.fromMilli(Milli(250));

/** Show an active/idle pill: an encoder only runs (and uses bandwidth) while a viewer is subscribed. */
function trackActive(parent: Effect, status: HTMLElement, active: Getter<boolean | undefined>) {
	parent.run((effect) => {
		const on = effect.get(active);
		status.style.display = "";
		status.textContent = on ? "active" : "idle";
		status.className = `stat-status stat-status--${on ? "active" : "idle"}`;
	});
}

/** The Stats tab: what we're capturing and publishing. */
export function statsTab(parent: Effect, publish: MoqPublish): HTMLElement {
	const container = DOM.create("div", { className: "tab-body" });

	// Video card: static detail as rows, live capture fps + upload bitrate as graphs.
	const videoCard = statCard("video", "Video", videoIcon);
	const vRes = statLine(videoCard.grid, "Resolution");
	const vCodec = statLine(videoCard.grid, "Codec");
	const vBitrateGraph = graph(parent, "Bitrate", { color: "#a855f7", format: formatBitrate });
	const vFpsGraph = graph(parent, "Frame rate", { color: "#facc15", format: formatFps });
	videoCard.el.append(vBitrateGraph.el, vFpsGraph.el);

	// active = a viewer is subscribed and we're actually encoding/sending.
	trackActive(parent, videoCard.status, publish.video.out.active);

	const audioCard = statCard("audio", "Audio", audioIcon);
	const aCodec = statLine(audioCard.grid, "Codec");
	const aRate = statLine(audioCard.grid, "Sample rate");
	const aChannels = statLine(audioCard.grid, "Channels");
	const aBitrate = statLine(audioCard.grid, "Bitrate");
	trackActive(parent, audioCard.status, publish.audio.out.active);

	const netCard = statCard("network", "Connection", wifiIcon);
	const nStatus = statLine(netCard.grid, "Status");
	const nServer = statLine(netCard.grid, "Server");
	const nName = statLine(netCard.grid, "Broadcast");

	container.append(videoCard.el, audioCard.el, netCard.el);

	// Resolution/codec from the live capture (display) + catalog; card hides when not capturing video.
	parent.run((effect) => {
		const capture = effect.get(publish.video.in.capture);
		const display = capture ? effect.get(capture.out.display) : undefined;
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
	// Counted off our own stream rather than a signal, which would coalesce a burst of frames into
	// one notification and undercount the rate.
	let frames = 0;
	parent.run((effect) => {
		const capture = effect.get(publish.video.in.capture);
		const fanout = capture ? effect.get(capture.out.frames) : undefined;
		if (!fanout) return;

		const reader = fanout.subscribe(effect).getReader();
		effect.cleanup(() => {
			reader.cancel().catch(() => {});
		});

		// Not raced against teardown, which would drop a frame whose read had already resolved.
		effect.spawn(async () => {
			for (;;) {
				const { value: frame } = await reader.read();
				if (!frame) break;

				frames++;
				frame.close();
			}
		});
	});

	const encoded = () => publish.video.out.stats.peek().bytes;

	let prevFrames = 0;
	let prevBytes = encoded();
	const at = () => Nano.toMilli(Nano(now()));
	let prevWhen = at();

	parent.interval(() => {
		const now = at();
		const elapsed = now - prevWhen;
		prevWhen = now;

		const frameDelta = frames - prevFrames;
		prevFrames = frames;
		vFpsGraph.push(elapsed > 0 && frameDelta > 0 ? frameDelta / (elapsed / 1000) : undefined);

		const bytes = encoded();
		const byteDelta = bytes - prevBytes;
		prevBytes = bytes;
		vBitrateGraph.push(elapsed > 0 && byteDelta > 0 ? (byteDelta * 8) / (elapsed / 1000) : undefined);
	}, POLL);

	return container;
}

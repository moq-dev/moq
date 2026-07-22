/**
 * MoQ watch inspector.
 *
 * We discover every broadcast announced under a prefix and render one tile per
 * live broadcast on the left: a `<moq-watch-ui>` (player chrome) wrapping a
 * `<moq-watch>` element. The right column shows live stats (catalog, decode,
 * network, metadata) for the *active* tile only, read straight off that tile's
 * `<moq-watch>` `broadcast`/decoder signals.
 *
 * Audio policy: only the active tile plays sound. Clicking a tile makes it
 * active (and is the user gesture that lets its audio start); every other tile
 * is muted.
 *
 * The per-stream metadata rides on a separate `meta.json` track within the active
 * broadcast (advertised in the catalog's `metadata` list); we subscribe to it off
 * the tile's `broadcast.out.active` consumer and decode it with @moq/json.
 */

import "./highlight";
import * as Json from "@moq/json";
import "@moq/watch/element"; // defines <moq-watch>
import "@moq/watch/ui"; // defines <moq-watch-ui>
import { Hang, Net, Signals } from "@moq/watch";
import type MoqWatch from "@moq/watch/element";
import MoqWatchSupport from "@moq/watch/support/element";
import { bufferBars, formatBitrate, formatFps, graph, renderRows } from "./viz";

/** Re-exported so bundlers keep the `<moq-watch-support>` element registration. */
export { MoqWatchSupport };

// Injected by Vite (see justfile). Defaults to the local relay.
const RELAY_URL = import.meta.env.VITE_RELAY_URL ?? "http://localhost:4443";

const $ = <T extends HTMLElement>(id: string): T => {
	const el = document.getElementById(id);
	if (!el) throw new Error(`missing #${id}`);
	return el as T;
};

function isDownloading(
	effect: Signals.Effect,
	watch: MoqWatch,
	decoder: MoqWatch["video"] | MoqWatch["audio"],
): boolean {
	return (
		effect.get(decoder.in.enabled) &&
		!!effect.get(decoder.source.out.track) &&
		!!effect.get(watch.broadcast.out.active)
	);
}

// Build a branded path from a user-typed prefix, tolerating a trailing slash
// (we show "demo/" in the UI but the path is "demo").
const prefixPath = (raw: string): Net.Path.Valid => Net.Path.from(raw.trim().replace(/\/+$/, ""));

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

// Empty prefix discovers every broadcast on the relay.
const prefixInput = new Signals.Signal("");

// Active broadcasts announced under the prefix (full paths), sorted.
const broadcasts = new Signals.Signal<string[]>([]);

// The active tile: the only one that plays audio. undefined => all muted.
const active = new Signals.Signal<string | undefined>(undefined);

// The active tile's <moq-watch> element, or undefined when nothing is active.
// The right-hand stats panel reads everything off this.
const activeWatch = new Signals.Signal<MoqWatch | undefined>(undefined);

// The decoded value of the active broadcast's `meta.json` track, or undefined when
// the broadcast advertises none.
const metaSignal = new Signals.Signal<unknown>(undefined);

// The relay URL, editable at runtime. Both the discovery connection and every
// tile's <moq-watch> follow it reactively.
const relayUrl = new Signals.Signal<URL | undefined>(new URL(RELAY_URL));

// Discovery connection (the tiles each open their own connection internally).
const connection = new Net.Connection.Reload({ url: relayUrl, enabled: true });

// ---------------------------------------------------------------------------
// Per-broadcast tile (a <moq-watch-ui> in the left column)
// ---------------------------------------------------------------------------

interface WatchTile {
	readonly name: string;
	readonly el: HTMLElement;
	readonly watch: MoqWatch;
	close(): void;
}

function createTile(name: string): WatchTile {
	const el = document.createElement("div");
	el.className =
		"rounded-lg overflow-hidden border border-neutral-800 bg-neutral-900 cursor-pointer transition-colors";

	const label = document.createElement("div");
	label.className =
		"flex items-center gap-2 px-3 py-1.5 text-xs font-mono text-neutral-300 border-b border-neutral-800";
	const labelText = document.createElement("span");
	labelText.className = "truncate";
	labelText.textContent = (Net.Path.stripPrefix(prefixPath(prefixInput.peek()), Net.Path.from(name)) ??
		name) as string;
	// Speaker badge marking the tile whose audio is playing (active + has audio).
	const audioBadge = document.createElement("span");
	audioBadge.className = "ml-auto shrink-0";
	audioBadge.textContent = "🔊";
	audioBadge.title = "audio active";
	audioBadge.hidden = true;
	label.append(labelText, audioBadge);

	// Each tile is a <moq-watch-ui> (player chrome: play/pause, volume,
	// fullscreen) wrapping a bare <moq-watch> that renders into its <canvas>
	// child. We still drive audio on the inner <moq-watch> and read its stats off
	// `broadcast` and decoders, so the shared inspector panel reflects the active tile.
	const watch = document.createElement("moq-watch") as MoqWatch;
	watch.name = name;
	watch.muted = true; // unmuted only while active (see below)
	// Default to a fixed 100ms jitter buffer (instead of adaptive "real-time") so
	// the latency visualization has something to show. Drag it in the panel.
	watch.setAttribute("latency", "100");
	const canvas = document.createElement("canvas");
	canvas.style.cssText = "width: 100%; height: auto;";
	watch.appendChild(canvas);

	const player = document.createElement("moq-watch-ui");
	player.appendChild(watch);
	el.append(label, player);

	const effects = new Signals.Effect();

	// Clicking anywhere in the tile makes it the active audio source. The click
	// doubles as the user gesture browsers require before audio can start.
	effects.event(el, "pointerdown", () => active.set(name));

	// Follow the editable relay URL in its own effect. Keeping this separate from
	// the active-state effect below is important: `watch.url =` reassigns a fresh
	// URL into the connection, which reconnects and flashes the canvas black. We
	// only want that when the URL actually changes, not on every active switch.
	effects.run((effect) => {
		watch.url = effect.get(relayUrl);
	});

	// Active state: only toggle audio + the active styling, so switching tiles
	// keeps the video playing. This depends on `active` alone: reading the catalog here
	// would re-assert `muted` on every catalog frame and clobber the player's mute button.
	effects.run((effect) => {
		const isActive = effect.get(active) === name;
		el.classList.toggle("border-emerald-500", isActive);
		el.classList.toggle("border-neutral-800", !isActive);
		watch.muted = !isActive;
	});

	// Show the speaker badge only while the active tile is downloading audio.
	effects.run((effect) => {
		const isActive = effect.get(active) === name;
		audioBadge.hidden = !(isActive && isDownloading(effect, watch, watch.audio));
	});

	return {
		name,
		el,
		watch,
		close() {
			effects.close();
			el.remove(); // disconnects <moq-watch> -> stops its connection
		},
	};
}

// ---------------------------------------------------------------------------
// Broadcast discovery
// ---------------------------------------------------------------------------
//
// Subscribe to announcements under the prefix and keep a live set of active broadcasts.
// `announced.next()` drains the update stream, so we track membership ourselves: active=true adds the
// path, active=false removes it. `Reload.announced()` spans reconnects (it retracts everything on
// disconnect and re-announces on reconnect), so the set self-heals without any extra wiring here.
const discovery = new Signals.Effect();
discovery.run((effect) => {
	const prefix = prefixPath(effect.get(prefixInput));
	const announced = connection.announced(prefix);
	effect.cleanup(() => announced.close());

	const live = new Set<string>();
	effect.spawn(async () => {
		for (;;) {
			const entry = await Promise.race([effect.cancel, announced.next()]);
			if (!entry) break;
			const path = Net.Path.join(prefix, entry.path);
			// Only `.hang` broadcasts are watchable streams; this skips the relay's
			// `.stats` broadcast (see the stats dashboard demo for that one).
			if (!path.endsWith(".hang")) continue;
			if (entry.active) live.add(path);
			else live.delete(path);
			broadcasts.set([...live].sort());
		}
	});
});

// ---------------------------------------------------------------------------
// Tile lifecycle: reconcile against discovery
// ---------------------------------------------------------------------------
//
// A persistent map outside the effect so re-runs reconcile (add new, close gone)
// rather than tearing every tile down.
const tiles = new Map<string, WatchTile>();
const playersContainer = $("players");

// Recompute the active tile's element from the current selection + tile map.
// Called from both the reconcile effect (tiles changed) and the selection effect
// (active changed) so `activeWatch` is never left pointing at a stale or
// not-yet-created tile.
function syncActiveWatch(): void {
	const name = active.peek();
	activeWatch.set(name ? tiles.get(name)?.watch : undefined);
}

const tilesEffect = new Signals.Effect();
tilesEffect.run((effect) => {
	const list = effect.get(broadcasts);
	const live = new Set(list);

	for (const [name, t] of tiles) {
		if (!live.has(name)) {
			t.close();
			tiles.delete(name);
		}
	}
	for (const name of list) {
		if (!tiles.has(name)) tiles.set(name, createTile(name));
	}
	// Keep DOM order matching the sorted list (append moves existing nodes).
	for (const name of list) {
		const t = tiles.get(name);
		if (t) playersContainer.append(t.el);
	}

	$("players-empty").hidden = list.length > 0;
	syncActiveWatch();
});

// ---------------------------------------------------------------------------
// Reactive UI
// ---------------------------------------------------------------------------

const ui = new Signals.Effect();

// Relay URL is editable: on commit, reconnect discovery + every tile to it.
const relayEl = $<HTMLInputElement>("relay-url");
relayEl.value = RELAY_URL;
relayEl.addEventListener("change", () => {
	try {
		relayUrl.set(new URL(relayEl.value.trim()));
	} catch {
		// Revert invalid input to the last good URL.
		relayEl.value = relayUrl.peek()?.toString() ?? RELAY_URL;
	}
});

const prefixEl = $<HTMLInputElement>("prefix");
prefixEl.value = prefixInput.peek();
prefixEl.addEventListener("input", () => prefixInput.set(prefixEl.value));

// Keep the active tile valid: auto-pick the first broadcast and switch away from
// one that disappears, but never steal focus once the user has chosen.
ui.run((effect) => {
	const list = effect.get(broadcasts);
	const cur = active.peek();
	if (cur && list.includes(cur)) return;
	active.set(list[0]);
});

// Point `activeWatch` at the selected tile's element whenever the selection
// changes (tile creation is handled by `tilesEffect`, also via syncActiveWatch).
ui.run((effect) => {
	effect.get(active);
	syncActiveWatch();
});

// Connection pill: Connected / Connecting / Disconnected.
ui.run((effect) => {
	const status = effect.get(connection.status); // connecting | connected | disconnected
	const label = status.charAt(0).toUpperCase() + status.slice(1);
	setPill(
		"conn-status",
		"conn-text",
		label,
		status === "connected" ? "ok" : status === "connecting" ? "wait" : "bad",
	);
});

// Broadcast pill: Online when the active broadcast is live, else Loading/Offline.
ui.run((effect) => {
	const watch = effect.get(activeWatch);
	const stream = watch ? effect.get(watch.broadcast.out.status) : "offline"; // offline | loading | live
	if (stream === "live") setPill("bcast-status", "bcast-text", "Online", "ok");
	else if (watch && stream === "loading") setPill("bcast-status", "bcast-text", "Loading", "wait");
	else setPill("bcast-status", "bcast-text", "Offline", "bad");
});

// Video section: catalog presence decides whether the card exists, but the
// details come from the selected source and only appear while video is downloaded.
ui.run((effect) => {
	const watch = effect.get(activeWatch);
	const video = watch ? effect.get(watch.video.source.out.catalog) : undefined;
	const section = $("video-section");
	if (!watch || !video) {
		section.hidden = true;
		return;
	}
	section.hidden = false;

	const downloading = isDownloading(effect, watch, watch.video);
	$("video-state").hidden = downloading;
	if (!downloading) {
		renderRows($("video-info"), []);
		return;
	}

	const stalled = effect.get(watch.video.out.stalled);
	const live = effect.get(watch.broadcast.out.status) === "live";
	const config = effect.get(watch.video.source.out.config);
	const display = effect.get(watch.video.out.display);

	const resolution =
		config?.codedWidth && config?.codedHeight
			? `${config.codedWidth}×${config.codedHeight}`
			: display
				? `${display.width}×${display.height}`
				: undefined;

	renderRows($("video-info"), [
		["codec", config?.codec],
		["resolution", resolution],
		["framerate", config?.framerate ? `${config.framerate} fps` : undefined],
		// A stall is mid-stream starvation, not "offline" - only surface it when live.
		["stalled", live && stalled ? "⚠️ recovering" : undefined],
	]);
});

// Audio section follows the same policy: the selected source describes what is
// active, while the muted state replaces those details when audio is not pulled.
ui.run((effect) => {
	const watch = effect.get(activeWatch);
	const audio = watch ? effect.get(watch.audio.source.out.catalog) : undefined;
	const section = $("audio-section");
	if (!watch || !audio) {
		section.hidden = true;
		return;
	}
	section.hidden = false;

	const downloading = isDownloading(effect, watch, watch.audio);
	$("audio-state").hidden = downloading;
	if (!downloading) {
		renderRows($("audio-info"), []);
		return;
	}

	const config = effect.get(watch.audio.source.out.config);
	const sampleRate = effect.get(watch.audio.out.sampleRate);
	renderRows($("audio-info"), [
		["codec", config?.codec],
		["sample rate", sampleRate ? `${sampleRate} Hz` : undefined],
		["channels", config?.numberOfChannels ? String(config.numberOfChannels) : undefined],
	]);
});

// Network section: only shown while connected to the relay with an active tile.
ui.run((effect) => {
	const connected = effect.get(connection.status) === "connected";
	const watch = effect.get(activeWatch);
	const section = $("network-section");
	if (!connected || !watch) {
		section.hidden = true;
		return;
	}
	section.hidden = false;

	// Report the transport negotiated by the live connection.
	const conn = effect.get(connection.established);
	$("network-transport").textContent = conn ? (conn.transport === "websocket" ? "WebSocket" : "WebTransport") : "";

	const video = effect.get(watch.video.out.stats);
	const audio = effect.get(watch.audio.out.stats);
	const bytes = (video?.bytesReceived ?? 0) + (audio?.bytesReceived ?? 0);
	renderRows($("network-info"), [["bytes received", bytes > 0 ? formatBytes(bytes) : undefined]]);
});

// Raw catalog (collapsible) - only rendered once the active catalog arrives.
ui.run((effect) => {
	const watch = effect.get(activeWatch);
	const catalog = watch ? effect.get(watch.broadcast.out.catalog) : undefined;
	const section = $("catalog-raw-section");
	if (!catalog) {
		section.hidden = true;
		return;
	}
	section.hidden = false;
	$("catalog-raw").textContent = JSON.stringify(catalog, null, 2);
});

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------
//
// The publish demo serves its metadata as a separate `meta.json` track, advertised
// in the catalog's `metadata` list. We read the active broadcast off
// `broadcast.out.active`, subscribe to that track, and decode the JSON value,
// re-subscribing whenever the broadcast (or the advertised track) changes.
// Memoize the advertised track name so the subscription below only re-runs when it
// (or the active broadcast) changes, not on every catalog frame (e.g. a live
// encoder-setting tweak rewrites the catalog).
const metaTrackName = ui.computed((effect) => {
	const watch = effect.get(activeWatch);
	if (!watch) return undefined;
	const catalog = effect.get(watch.broadcast.out.catalog) as { metadata?: string[] } | undefined;
	return catalog?.metadata?.[0];
});

ui.run((effect) => {
	const watch = effect.get(activeWatch);
	const broadcast = watch ? effect.get(watch.broadcast.out.active) : undefined;
	const trackName = effect.get(metaTrackName);
	if (!broadcast || !trackName) {
		metaSignal.set(undefined);
		return;
	}

	const track = broadcast.track(trackName).subscribe({ priority: Hang.Catalog.PRIORITY.catalog });
	effect.cleanup(() => track.close());
	const consumer = new Json.Snapshot.Consumer<unknown>(track);

	effect.spawn(async () => {
		try {
			for (;;) {
				const value = await Promise.race([effect.cancel, consumer.next()]);
				if (value === undefined) break;
				metaSignal.set(value);
			}
		} catch (err) {
			console.warn("error reading metadata", err);
		} finally {
			metaSignal.set(undefined);
		}
	});
});

// Metadata view - only shown when the active broadcast is live AND has actually
// received a frame (no placeholder text while offline).
ui.run((effect) => {
	const meta = effect.get(metaSignal);
	const watch = effect.get(activeWatch);
	const live = watch ? effect.get(watch.broadcast.out.status) === "live" : false;
	const section = $("metadata-section");
	const pre = $("metadata");
	if (live && meta !== undefined) {
		section.hidden = false;
		pre.textContent = JSON.stringify(meta, null, 2);
	} else {
		section.hidden = true;
		pre.textContent = "";
	}
});

// ---------------------------------------------------------------------------
// Live graphs (bitrate / frame rate / RTT) + buffer visualization
// ---------------------------------------------------------------------------
//
// These are stateful DOM elements, so we build them once and feed them from a
// single timer that samples the *active* tile, rather than rebuilding per render.

const viz = new Signals.Effect();

// Each media card reports measured bitrate. Network throughput is their sum.
const videoBitrateGraph = graph(viz, "Bitrate", { color: "#a855f7", format: formatBitrate });
const fpsGraph = graph(viz, "Frame rate", { color: "#facc15", format: formatFps });
$("video-graphs").append(videoBitrateGraph.el, fpsGraph.el);

const audioBitrateGraph = graph(viz, "Bitrate", { color: "#fb7185", format: formatBitrate });
$("audio-graphs").append(audioBitrateGraph.el);

const throughputGraph = graph(viz, "Throughput", { color: "#34d399", format: formatBitrate });
const rttGraph = graph(viz, "Round trip", { color: "#38bdf8", format: (v) => `${Math.round(v)} ms` });
$("network-graphs").append(throughputGraph.el, rttGraph.el);

const allGraphs = [videoBitrateGraph, audioBitrateGraph, fpsGraph, throughputGraph, rttGraph];

// Sample the active tile's byte/frame counters and push per-second rates.
let prevWatch: MoqWatch | undefined;
let prev = { frames: 0, videoBytes: 0, audioBytes: 0, when: performance.now() };
viz.interval(() => {
	const watch = activeWatch.peek();
	const now = performance.now();

	// Reset baselines when switching tiles (or when idle) so the first sample
	// isn't a huge spike from the counter difference.
	if (watch !== prevWatch || !watch) {
		prevWatch = watch;
		const video = watch?.video.out.stats.peek();
		const audio = watch?.audio.out.stats.peek();
		prev = {
			frames: video?.frameCount ?? 0,
			videoBytes: video?.bytesReceived ?? 0,
			audioBytes: audio?.bytesReceived ?? 0,
			when: now,
		};
		for (const g of allGraphs) g.push(undefined);
		return;
	}

	const v = watch.video.out.stats.peek();
	const a = watch.audio.out.stats.peek();
	const videoBytes = v?.bytesReceived ?? 0;
	const audioBytes = a?.bytesReceived ?? 0;
	const frames = v?.frameCount ?? 0;
	const elapsed = now - prev.when;

	const perSec = (delta: number) => (delta >= 0 ? (delta * 1000) / elapsed : undefined);
	let videoBitrate: number | undefined;
	let audioBitrate: number | undefined;
	let throughput: number | undefined;
	let fps: number | undefined;
	if (elapsed > 0) {
		videoBitrate = perSec((videoBytes - prev.videoBytes) * 8);
		audioBitrate = perSec((audioBytes - prev.audioBytes) * 8);
		throughput = perSec((videoBytes - prev.videoBytes + audioBytes - prev.audioBytes) * 8);
		fps = perSec(frames - prev.frames);
	}
	videoBitrateGraph.push(videoBitrate);
	audioBitrateGraph.push(audioBitrate);
	fpsGraph.push(fps);
	throughputGraph.push(throughput);

	const rtt = watch.connection.stats.peek()?.rtt as unknown as number | undefined;
	rttGraph.push(rtt && rtt > 0 ? rtt : undefined);

	prev = { frames, videoBytes, audioBytes, when: now };
}, 250);

// Rebuild the buffer visualization whenever the active tile changes; it binds to
// one element and runs its own animation loop until its child effect closes.
ui.run((effect) => {
	const watch = effect.get(activeWatch);
	const live = watch ? effect.get(watch.broadcast.out.status) === "live" : false;
	const section = $("buffer-section");
	const host = $("buffer-viz");
	host.replaceChildren();
	if (!watch || !live) {
		section.hidden = true;
		return;
	}
	section.hidden = false;
	const child = new Signals.Effect();
	effect.cleanup(() => child.close());
	host.append(bufferBars(child, watch));
});

// ---------------------------------------------------------------------------
// Small render helpers
// ---------------------------------------------------------------------------

function setPill(statusId: string, textId: string, label: string, state: "ok" | "wait" | "bad"): void {
	$(textId).textContent = label;
	const dot = $(statusId).querySelector(".dot") as HTMLElement;
	const color = state === "ok" ? "bg-emerald-500" : state === "wait" ? "bg-amber-400" : "bg-red-500";
	dot.className = `dot w-2 h-2 rounded-full ${color}`;
}

function formatBytes(n: number): string {
	if (n < 1024) return `${n} B`;
	if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
	return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

// Vite re-evaluates this module on hot reload, dropping the references to the
// module-scoped effects/connection above. Close them on dispose so they don't
// get garbage collected unclosed (which the signals library warns about).
if (import.meta.hot) {
	import.meta.hot.dispose(() => {
		for (const effect of [discovery, tilesEffect, ui, viz]) effect.close();
		for (const tile of tiles.values()) tile.close();
		tiles.clear();
		connection.close();
	});
}

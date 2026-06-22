/**
 * Stats visualizations for the watch inspector, ported from the private internals
 * of `<moq-watch-ui>` so the demo is self-contained. They read only the public
 * `MoqWatch.backend` signals, so this doubles as an example of building your own
 * charts on top of the API.
 *
 * - `graph()` is a rolling sparkline (used for bitrate / frame rate).
 * - `bufferBars()` is an editable view of the video/audio jitter buffer; drag it
 *   to change the latency target.
 */

import type { BufferedRanges, Signals } from "@moq/watch";
import type MoqWatch from "@moq/watch/element";

export interface GraphOptions {
	/** Fixed y-axis maximum. If omitted, the graph autoscales to its rolling peak. */
	max?: number;
	/** How many samples of history to retain (older samples scroll off the left). */
	samples?: number;
	/** Stroke/fill color for the line (any CSS color). */
	color?: string;
	/** Formats the latest value for the readout in the corner. */
	format?: (v: number) => string;
}

export interface Graph {
	/** The graph's root element; append it where you want the chart to render. */
	el: HTMLElement;
	/** Append a sample. Pass undefined to record a gap (drawn as zero). */
	push(value: number | undefined): void;
}

const DEFAULT_SAMPLES = 120;

/** Normalize any CSS color and apply an alpha, so the gradient works for named/rgb/hsl inputs too. */
function withAlpha(color: string, alpha: number): string {
	const ctx = document.createElement("canvas").getContext("2d");
	if (!ctx) return color;
	ctx.fillStyle = color;
	const normalized = ctx.fillStyle;
	if (normalized.startsWith("#")) {
		const n = Number.parseInt(normalized.slice(1), 16);
		return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${alpha})`;
	}
	const parts = normalized.match(/[\d.]+/g);
	if (parts && parts.length >= 3) return `rgba(${parts[0]}, ${parts[1]}, ${parts[2]}, ${alpha})`;
	return color;
}

/**
 * A rolling time-series sparkline. Samples scroll right-to-left and the area
 * under the line is filled with a fading gradient. Redraws are event-driven:
 * each `push` and any canvas resize triggers a repaint (no animation loop).
 */
export function graph(parent: Signals.Effect, title: string, opts?: GraphOptions): Graph {
	const color = opts?.color ?? "#4ade80";
	const fillTop = withAlpha(color, 0.33);
	const fillBottom = withAlpha(color, 0);
	const capacity = Number.isFinite(opts?.samples)
		? Math.max(1, Math.floor(opts?.samples as number))
		: DEFAULT_SAMPLES;

	const el = document.createElement("div");

	const header = document.createElement("div");
	header.className = "flex justify-between text-xs text-neutral-400";
	const label = document.createElement("span");
	label.textContent = title;
	const value = document.createElement("span");
	value.className = "font-mono";
	value.style.color = color;
	value.textContent = "—";
	header.append(label, value);

	const canvas = document.createElement("canvas");
	canvas.style.cssText = "display: block; width: 100%; height: 40px;";

	el.append(header, canvas);

	const samples: number[] = [];
	let scale = opts?.max ?? 1;

	const draw = () => {
		const ctx = canvas.getContext("2d");
		if (!ctx) return;
		const dpr = window.devicePixelRatio || 1;
		const rect = canvas.getBoundingClientRect();
		const w = rect.width;
		const h = rect.height;
		const cw = Math.round(w * dpr);
		const ch = Math.round(h * dpr);
		if (canvas.width !== cw || canvas.height !== ch) {
			canvas.width = cw;
			canvas.height = ch;
		}

		ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
		ctx.clearRect(0, 0, w, h);
		if (w <= 0 || h <= 0 || samples.length <= 1) return;

		const peak = Math.max(...samples);
		const target = opts?.max ?? Math.max(1, peak * 1.2);
		scale += (target - scale) * 0.1;

		const pad = 1;
		const usable = h - pad * 2;
		const x = (i: number) => (capacity <= 1 ? w : (i / (capacity - 1)) * w);
		const y = (v: number) => pad + usable - (Math.min(v, scale) / scale) * usable;
		const offset = capacity - samples.length;

		ctx.beginPath();
		ctx.moveTo(x(offset), y(samples[0]));
		for (let i = 1; i < samples.length; i++) ctx.lineTo(x(offset + i), y(samples[i]));
		const grad = ctx.createLinearGradient(0, 0, 0, h);
		grad.addColorStop(0, fillTop);
		grad.addColorStop(1, fillBottom);
		ctx.save();
		ctx.lineTo(x(offset + samples.length - 1), h);
		ctx.lineTo(x(offset), h);
		ctx.closePath();
		ctx.fillStyle = grad;
		ctx.fill();
		ctx.restore();

		ctx.beginPath();
		ctx.moveTo(x(offset), y(samples[0]));
		for (let i = 1; i < samples.length; i++) ctx.lineTo(x(offset + i), y(samples[i]));
		ctx.strokeStyle = color;
		ctx.lineWidth = 1.5;
		ctx.lineJoin = "round";
		ctx.stroke();
	};

	const push = (v: number | undefined) => {
		samples.push(v !== undefined && Number.isFinite(v) ? Math.max(0, v) : 0);
		while (samples.length > capacity) samples.shift();
		value.textContent = v !== undefined && Number.isFinite(v) ? (opts?.format?.(v) ?? v.toFixed(0)) : "—";
		draw();
	};

	if (typeof ResizeObserver !== "undefined") {
		const observer = new ResizeObserver(() => draw());
		observer.observe(canvas);
		parent.cleanup(() => observer.disconnect());
	}

	return { el, push };
}

const BUFFER_MAX = 4000; // window shown, in milliseconds

/** Draw one track's buffered ranges relative to the current playhead (timestamp). */
function drawRanges(
	canvas: HTMLCanvasElement,
	ranges: BufferedRanges,
	timestamp: number | undefined,
	stalled: boolean,
) {
	const ctx = canvas.getContext("2d");
	if (!ctx) return;

	const dpr = window.devicePixelRatio || 1;
	const rect = canvas.getBoundingClientRect();
	const width = rect.width;
	const height = rect.height;
	const cw = Math.round(width * dpr);
	const ch = Math.round(height * dpr);
	if (canvas.width !== cw || canvas.height !== ch) {
		canvas.width = cw;
		canvas.height = ch;
	}

	ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
	ctx.clearRect(0, 0, width, height);
	if (timestamp === undefined) return;

	for (let i = 0; i < ranges.length; i++) {
		const range = ranges[i];
		const startMs = range.start - timestamp;
		const endMs = range.end - timestamp;
		const visibleStart = Math.max(0, startMs);
		const visibleEnd = Math.min(endMs, BUFFER_MAX);
		if (visibleEnd <= visibleStart) continue;

		const x = (visibleStart / BUFFER_MAX) * width;
		const w = Math.max(2, ((visibleEnd - visibleStart) / BUFFER_MAX) * width);

		ctx.globalAlpha = 0.85;
		// red while buffering, yellow for extra ranges, green for the main one.
		ctx.fillStyle = stalled ? "#f87171" : i > 0 ? "#facc15" : "#4ade80";
		if (typeof ctx.roundRect === "function") {
			ctx.beginPath();
			ctx.roundRect(x, 1, w, height - 2, 2);
			ctx.fill();
		} else {
			ctx.fillRect(x, 1, w, height - 2);
		}
	}
}

const STEP = 10; // latency drag/keyboard granularity, in milliseconds

/**
 * Editable latency visualization: video + audio jitter buffers drawn as bars
 * from the playhead (left edge) out to ~4s, with the current latency target
 * marked. Drag (or focus + arrow keys) to set the buffer, like `<moq-watch-ui>`.
 * Each call binds to one `watch`; close the parent effect to stop it.
 */
export function bufferBars(parent: Signals.Effect, watch: MoqWatch): HTMLElement {
	const root = document.createElement("div");

	const viz = document.createElement("div");
	viz.style.cssText = "position: relative; cursor: ew-resize;";
	viz.tabIndex = 0;
	viz.setAttribute("role", "slider");
	viz.setAttribute("aria-label", "Latency target");
	viz.setAttribute("aria-valuemin", "0");
	viz.setAttribute("aria-valuemax", String(BUFFER_MAX));

	const mkTrack = (name: string) => {
		const row = document.createElement("div");
		row.className = "flex items-center gap-2";
		const label = document.createElement("span");
		label.className = "w-10 shrink-0 text-[10px] text-neutral-500";
		label.textContent = name;
		const canvas = document.createElement("canvas");
		canvas.style.cssText = "display: block; flex: 1; height: 20px;";
		row.append(label, canvas);
		return { row, canvas };
	};

	const video = mkTrack("video");
	const audio = mkTrack("audio");

	// Vertical target line over the canvas region (offset past the label + gap).
	const target = document.createElement("div");
	target.style.cssText = "position: absolute; top: 0; bottom: 0; width: 2px; background: #fff; pointer-events: none;";
	const targetLabel = document.createElement("span");
	targetLabel.className = "text-[10px] text-neutral-300";
	targetLabel.style.cssText = "position: absolute; top: -2px; left: 4px; white-space: nowrap;";
	target.appendChild(targetLabel);

	// pointer-events: none so clicks pass through to `viz` for the drag handler.
	const canvasArea = document.createElement("div");
	canvasArea.style.cssText = "position: absolute; left: 3rem; right: 0; top: 0; bottom: 0; pointer-events: none;";
	canvasArea.appendChild(target);

	const space = document.createElement("div");
	space.className = "space-y-1";
	space.append(video.row, audio.row);
	viz.append(space, canvasArea);

	const legend = document.createElement("div");
	legend.className = "mt-2 text-[10px] text-neutral-500";
	legend.textContent = "buffered ahead of the playhead; drag to change the latency target";

	root.append(viz, legend);

	// Set the latency floor, leaving the ceiling. Cast bypasses the branded ms type.
	const setLatency = (ms: number) => {
		const clamped = Math.max(0, Math.min(BUFFER_MAX, ms));
		watch.latencyMin = clamped as unknown as typeof watch.latencyMin;
	};

	const setFromX = (clientX: number) => {
		const rect = canvasArea.getBoundingClientRect();
		if (rect.width <= 0) return;
		const x = Math.max(0, Math.min(clientX - rect.left, rect.width));
		const ms = (x / rect.width) * BUFFER_MAX;
		setLatency(Math.round(ms / STEP) * STEP);
	};

	let dragging = false;
	parent.event(viz, "mousedown", (e) => {
		dragging = true;
		setFromX(e.clientX);
	});
	parent.event(document, "mousemove", (e) => {
		if (dragging) setFromX(e.clientX);
	});
	parent.event(document, "mouseup", () => {
		dragging = false;
	});
	parent.event(viz, "keydown", (e) => {
		const delta =
			e.key === "ArrowRight" || e.key === "ArrowUp"
				? STEP
				: e.key === "ArrowLeft" || e.key === "ArrowDown"
					? -STEP
					: 0;
		if (delta === 0) return;
		e.preventDefault();
		setLatency((watch.backend.jitter.peek() as unknown as number) + delta);
	});

	// Position the target line from the live jitter (actual measured buffer in ms).
	parent.run((effect) => {
		const jitter = effect.get(watch.backend.jitter) as unknown as number;
		const pct = Math.max(0, Math.min(1, jitter / BUFFER_MAX)) * 100;
		target.style.left = `${pct}%`;
		targetLabel.textContent = `${Math.round(jitter)}ms`;
		viz.setAttribute("aria-valuenow", String(Math.round(jitter)));
	});

	// Repaint the bars every animation frame; cleaned up when the effect closes.
	const draw = () => {
		const timestamp = watch.backend.sync.now() as number | undefined;
		const stalled = watch.backend.video.stalled.peek();
		drawRanges(video.canvas, watch.backend.video.buffered.peek(), timestamp, stalled);
		drawRanges(audio.canvas, watch.backend.audio.buffered.peek(), timestamp, false);
		parent.animate(draw);
	};
	parent.animate(draw);

	return root;
}

/** Format a bits-per-second value as kbps / Mbps. */
export function formatBitrate(bps: number): string {
	if (bps >= 1_000_000) return `${(bps / 1_000_000).toFixed(1)} Mbps`;
	return `${Math.round(bps / 1000)} kbps`;
}

/** Format frames-per-second. */
export function formatFps(v: number): string {
	return `${v.toFixed(0)} fps`;
}

/** A key/value row for the stat panels. */
export function kv(key: string, value: string): HTMLElement {
	const row = document.createElement("div");
	row.className = "flex justify-between gap-4 text-sm";
	const k = document.createElement("span");
	k.className = "text-neutral-400";
	k.textContent = key;
	const v = document.createElement("span");
	v.className = "font-mono text-neutral-100 text-right break-all";
	v.textContent = value;
	row.append(k, v);
	return row;
}

/** Render key/value rows, skipping any whose value is undefined (we don't show a stat we don't know). */
export function renderRows(container: HTMLElement, rows: [string, string | undefined][]): void {
	const known = rows.filter((r): r is [string, string] => r[1] !== undefined);
	container.replaceChildren(...known.map(([k, v]) => kv(k, v)));
}

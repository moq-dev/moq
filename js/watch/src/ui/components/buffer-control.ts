import * as Moq from "@moq/net";
import type { Effect } from "@moq/signals";
import type { BufferedRanges } from "../..";
import type MoqWatch from "../../element";

const MIN_RANGE = Moq.Time.Milli(0);
const RANGE_STEP = Moq.Time.Milli(10);
const DEFAULT_MAX = Moq.Time.Milli(4000);
const LABEL_WIDTH = 48;

function drawRanges(
	canvas: HTMLCanvasElement,
	ranges: BufferedRanges,
	timestamp: Moq.Time.Milli | undefined,
	max: Moq.Time.Milli,
	isBuffering: boolean,
) {
	const ctx = canvas.getContext("2d");
	if (!ctx) return;

	const dpr = window.devicePixelRatio || 1;
	const rect = canvas.getBoundingClientRect();
	const width = rect.width;
	const height = rect.height;

	const canvasW = Math.round(width * dpr);
	const canvasH = Math.round(height * dpr);
	if (canvas.width !== canvasW || canvas.height !== canvasH) {
		canvas.width = canvasW;
		canvas.height = canvasH;
	}

	ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
	ctx.clearRect(0, 0, width, height);

	if (timestamp === undefined) return;

	const padding = 2;
	const rangeHeight = height - padding * 2;
	const radius = 2;

	for (let i = 0; i < ranges.length; i++) {
		const range = ranges[i];
		const startMs = Moq.Time.Milli(range.start - timestamp);
		const endMs = Moq.Time.Milli(range.end - timestamp);
		const visibleStart = Math.max(0, startMs);
		const visibleEnd = Math.min(endMs, max);

		if (visibleEnd <= visibleStart) continue;

		const x = (visibleStart / max) * width;
		const w = Math.max(2, ((visibleEnd - visibleStart) / max) * width);

		ctx.globalAlpha = 0.85;
		ctx.fillStyle = isBuffering ? "#f87171" : i > 0 ? "#facc15" : "#4ade80";

		if (typeof ctx.roundRect === "function") {
			ctx.beginPath();
			ctx.roundRect(x, padding, w, rangeHeight, radius);
			ctx.fill();
		} else {
			ctx.fillRect(x, padding, w, rangeHeight);
		}

		if (endMs > max) {
			const overflowSec = ((endMs - max) / 1000).toFixed(1);
			ctx.globalAlpha = 0.7;
			ctx.fillStyle = "black";
			ctx.font = "500 9px system-ui, sans-serif";
			ctx.textAlign = "right";
			ctx.textBaseline = "middle";
			ctx.fillText(`+${overflowSec}s`, x + w - 4, height / 2);
		}
	}
}

export function bufferControl(parent: Effect, watch: MoqWatch, max: Moq.Time.Milli = DEFAULT_MAX): HTMLElement {
	const wrapper = document.createElement("div");
	wrapper.className = "buffer";

	const viz = document.createElement("div");
	viz.className = "buffer-visualization";
	viz.setAttribute("role", "slider");
	viz.tabIndex = 0;
	viz.setAttribute("aria-valuemin", MIN_RANGE.toString());
	viz.setAttribute("aria-valuemax", max.toString());
	viz.setAttribute("aria-label", "Buffer jitter");

	const playhead = document.createElement("div");
	playhead.className = "buffer-playhead";

	const videoTrack = document.createElement("div");
	videoTrack.className = "buffer-track buffer-track--video";
	const videoLabel = document.createElement("span");
	videoLabel.className = "buffer-track-label";
	videoLabel.textContent = "Video";
	const videoCanvas = document.createElement("canvas");
	videoCanvas.className = "buffer-canvas";
	videoTrack.append(videoLabel, videoCanvas);

	const audioTrack = document.createElement("div");
	audioTrack.className = "buffer-track buffer-track--audio";
	const audioLabel = document.createElement("span");
	audioLabel.className = "buffer-track-label";
	audioLabel.textContent = "Audio";
	const audioCanvas = document.createElement("canvas");
	audioCanvas.className = "buffer-canvas";
	audioTrack.append(audioLabel, audioCanvas);

	const targetArea = document.createElement("div");
	targetArea.className = "buffer-target-area";
	const targetLine = document.createElement("div");
	targetLine.className = "buffer-target-line";
	const targetLabel = document.createElement("span");
	targetLabel.className = "buffer-target-label";
	targetLine.appendChild(targetLabel);
	targetArea.appendChild(targetLine);

	const help = document.createElement("span");
	help.className = "buffer-help";
	help.textContent = "click to change latency";

	viz.append(playhead, videoTrack, audioTrack, targetArea, help);
	wrapper.appendChild(viz);

	let dragging = false;
	let hasInteracted = false;

	parent.run((effect) => {
		const jitter = effect.get(watch.sync.out.jitter);
		const pct = (jitter / max) * 100;
		targetLine.style.left = `${pct}%`;
		targetLabel.textContent = `${Math.round(jitter)}ms`;
		viz.setAttribute("aria-valuenow", jitter.toString());
	});

	const updateFromX = (clientX: number) => {
		const rect = viz.getBoundingClientRect();
		const trackWidth = rect.width - LABEL_WIDTH;
		const x = Math.max(0, Math.min(clientX - rect.left - LABEL_WIDTH, trackWidth));
		const ms = (x / trackWidth) * max;
		const snapped = Moq.Time.Milli(Math.round(ms / RANGE_STEP) * RANGE_STEP);
		const clamped = Moq.Time.Milli(Math.max(MIN_RANGE, Math.min(max, snapped)));
		watch.latencyMin = clamped;
	};

	const interact = () => {
		if (!hasInteracted) {
			hasInteracted = true;
			help.style.display = "none";
		}
	};

	parent.event(viz, "mousedown", (e) => {
		dragging = true;
		viz.classList.add("buffer-visualization--dragging");
		interact();
		updateFromX(e.clientX);
	});

	parent.event(document, "mousemove", (e) => {
		if (dragging) updateFromX(e.clientX);
	});

	parent.event(document, "mouseup", () => {
		if (!dragging) return;
		dragging = false;
		viz.classList.remove("buffer-visualization--dragging");
	});

	parent.event(viz, "keydown", (e) => {
		let delta = Moq.Time.Milli(0);
		if (e.key === "ArrowRight" || e.key === "ArrowUp") {
			delta = RANGE_STEP;
		} else if (e.key === "ArrowLeft" || e.key === "ArrowDown") {
			delta = Moq.Time.Milli(-RANGE_STEP);
		} else {
			return;
		}
		e.preventDefault();
		interact();
		const current = watch.sync.out.jitter.peek();
		const value = Moq.Time.Milli(Math.max(MIN_RANGE, Math.min(max, current + delta)));
		watch.latencyMin = value;
	});

	const draw = () => {
		const timestamp = watch.sync.now();
		const isBuffering = watch.video.out.stalled.peek();
		drawRanges(videoCanvas, watch.video.out.buffered.peek(), timestamp, max, isBuffering);
		drawRanges(audioCanvas, watch.audio.out.buffered.peek(), timestamp, max, isBuffering);
		parent.animate(draw);
	};
	parent.animate(draw);

	return wrapper;
}

import type { Moq } from "@moq/hang";
import type { Effect } from "@moq/signals";
import type { BufferedRanges } from "../..";
import type MoqWatch from "../../element";

const MIN_RANGE = 0 as Moq.Time.Milli;
const RANGE_STEP = 10 as Moq.Time.Milli;
const DEFAULT_MAX = 4000 as Moq.Time.Milli;
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
		const startMs = (range.start - timestamp) as Moq.Time.Milli;
		const endMs = (range.end - timestamp) as Moq.Time.Milli;
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
	wrapper.className = "watch-ui__buffer";

	const viz = document.createElement("div");
	viz.className = "watch-ui__buffer-visualization";
	viz.setAttribute("role", "slider");
	viz.tabIndex = 0;
	viz.setAttribute("aria-valuemin", MIN_RANGE.toString());
	viz.setAttribute("aria-valuemax", max.toString());
	viz.setAttribute("aria-label", "Buffer jitter");

	const playhead = document.createElement("div");
	playhead.className = "watch-ui__buffer-playhead";

	const videoTrack = document.createElement("div");
	videoTrack.className = "watch-ui__buffer-track watch-ui__buffer-track--video";
	const videoLabel = document.createElement("span");
	videoLabel.className = "watch-ui__buffer-track-label";
	videoLabel.textContent = "Video";
	const videoCanvas = document.createElement("canvas");
	videoCanvas.className = "watch-ui__buffer-canvas";
	videoTrack.append(videoLabel, videoCanvas);

	const audioTrack = document.createElement("div");
	audioTrack.className = "watch-ui__buffer-track watch-ui__buffer-track--audio";
	const audioLabel = document.createElement("span");
	audioLabel.className = "watch-ui__buffer-track-label";
	audioLabel.textContent = "Audio";
	const audioCanvas = document.createElement("canvas");
	audioCanvas.className = "watch-ui__buffer-canvas";
	audioTrack.append(audioLabel, audioCanvas);

	const targetArea = document.createElement("div");
	targetArea.className = "watch-ui__buffer-target-area";
	const targetLine = document.createElement("div");
	targetLine.className = "watch-ui__buffer-target-line";
	const targetLabel = document.createElement("span");
	targetLabel.className = "watch-ui__buffer-target-label";
	targetLine.appendChild(targetLabel);
	targetArea.appendChild(targetLine);

	const help = document.createElement("span");
	help.className = "watch-ui__buffer-help";
	help.textContent = "click to change latency";

	viz.append(playhead, videoTrack, audioTrack, targetArea, help);
	wrapper.appendChild(viz);

	let dragging = false;
	let hasInteracted = false;

	parent.run((effect) => {
		const jitter = effect.get(watch.backend.jitter);
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
		const snapped = (Math.round(ms / RANGE_STEP) * RANGE_STEP) as Moq.Time.Milli;
		const clamped = Math.max(MIN_RANGE, Math.min(max, snapped)) as Moq.Time.Milli;
		watch.backend.latency.set(clamped);
	};

	const interact = () => {
		if (!hasInteracted) {
			hasInteracted = true;
			help.style.display = "none";
		}
	};

	parent.event(viz, "mousedown", (e) => {
		dragging = true;
		viz.classList.add("watch-ui__buffer-visualization--dragging");
		interact();
		updateFromX(e.clientX);
	});

	parent.event(document, "mousemove", (e) => {
		if (dragging) updateFromX(e.clientX);
	});

	parent.event(document, "mouseup", () => {
		if (!dragging) return;
		dragging = false;
		viz.classList.remove("watch-ui__buffer-visualization--dragging");
	});

	parent.event(viz, "keydown", (e) => {
		let delta = 0 as Moq.Time.Milli;
		if (e.key === "ArrowRight" || e.key === "ArrowUp") {
			delta = RANGE_STEP;
		} else if (e.key === "ArrowLeft" || e.key === "ArrowDown") {
			delta = -RANGE_STEP as Moq.Time.Milli;
		} else {
			return;
		}
		e.preventDefault();
		interact();
		const current = watch.backend.jitter.peek();
		const value = Math.max(MIN_RANGE, Math.min(max, current + delta)) as Moq.Time.Milli;
		watch.backend.latency.set(value);
	});

	const draw = () => {
		const timestamp = watch.backend.sync.now();
		const isBuffering = watch.backend.video.stalled.peek();
		drawRanges(videoCanvas, watch.backend.video.buffered.peek(), timestamp, max, isBuffering);
		drawRanges(audioCanvas, watch.backend.audio.buffered.peek(), timestamp, max, isBuffering);
		parent.animate(draw);
	};
	parent.animate(draw);

	return wrapper;
}

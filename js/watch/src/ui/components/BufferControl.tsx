import type { Moq } from "@moq/hang";
import { createMemo, createSignal, onCleanup, onMount } from "solid-js";
import type { BufferedRanges } from "../..";
import useWatchUIContext from "../hooks/use-watch-ui";

const MIN_RANGE = 0 as Moq.Time.Milli;
const RANGE_STEP = 100 as Moq.Time.Milli;

type BufferControlProps = {
	/** Maximum buffer range in milliseconds (default: 5000ms = 5s) */
	max?: Moq.Time.Milli;
};

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

	// Resize canvas backing store if needed
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

		// Draw overflow label if the range extends past the visible max
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

export default function BufferControl(props: BufferControlProps) {
	const context = useWatchUIContext();
	const maxRange = (): Moq.Time.Milli => props.max ?? (5000 as Moq.Time.Milli);
	const [isDragging, setIsDragging] = createSignal(false);
	const [hasInteracted, setHasInteracted] = createSignal(false);

	const bufferTargetPct = createMemo(() => (context.jitter() / maxRange()) * 100);

	// Handle mouse interaction to set buffer via clicking/dragging on the visualization
	let containerRef: HTMLDivElement | undefined;
	let videoCanvasRef: HTMLCanvasElement | undefined;
	let audioCanvasRef: HTMLCanvasElement | undefined;

	const LABEL_WIDTH = 48; // px reserved for track labels

	const updateBufferFromMouseX = (clientX: number) => {
		if (!containerRef) return;
		const rect = containerRef.getBoundingClientRect();
		const trackWidth = rect.width - LABEL_WIDTH;
		const x = Math.max(0, Math.min(clientX - rect.left - LABEL_WIDTH, trackWidth));
		const ms = (x / trackWidth) * maxRange();
		const snapped = (Math.round(ms / RANGE_STEP) * RANGE_STEP) as Moq.Time.Milli;
		const clamped = Math.max(MIN_RANGE, Math.min(maxRange(), snapped)) as Moq.Time.Milli;
		context.setJitter(clamped);
	};

	const onMouseDown = (e: MouseEvent) => {
		setIsDragging(true);
		setHasInteracted(true);
		updateBufferFromMouseX(e.clientX);
		document.addEventListener("mousemove", onMouseMove);
		document.addEventListener("mouseup", onMouseUp);
	};

	const onMouseMove = (e: MouseEvent) => {
		if (isDragging()) {
			updateBufferFromMouseX(e.clientX);
		}
	};

	const onMouseUp = () => {
		setIsDragging(false);
		document.removeEventListener("mousemove", onMouseMove);
		document.removeEventListener("mouseup", onMouseUp);
	};

	const onKeyDown = (e: KeyboardEvent) => {
		let delta = 0 as Moq.Time.Milli;
		if (e.key === "ArrowRight" || e.key === "ArrowUp") {
			delta = RANGE_STEP;
		} else if (e.key === "ArrowLeft" || e.key === "ArrowDown") {
			delta = -RANGE_STEP as Moq.Time.Milli;
		} else {
			return;
		}
		e.preventDefault();
		setHasInteracted(true);
		const value = Math.max(MIN_RANGE, Math.min(maxRange(), context.jitter() + delta)) as Moq.Time.Milli;
		context.setJitter(value);
	};

	// Paint buffer ranges to canvas on each animation frame.
	// Reading signals outside a SolidJS tracking scope avoids reactive subscriptions entirely.
	let rafId: number | undefined;

	const draw = () => {
		const max = maxRange();
		const timestamp = context.timestamp();
		const buffering = context.buffering();

		if (videoCanvasRef) {
			drawRanges(videoCanvasRef, context.videoBuffered(), timestamp, max, buffering);
		}
		if (audioCanvasRef) {
			drawRanges(audioCanvasRef, context.audioBuffered(), timestamp, max, buffering);
		}

		rafId = requestAnimationFrame(draw);
	};

	onMount(() => {
		rafId = requestAnimationFrame(draw);
	});

	onCleanup(() => {
		if (rafId !== undefined) cancelAnimationFrame(rafId);
		document.removeEventListener("mousemove", onMouseMove);
		document.removeEventListener("mouseup", onMouseUp);
	});

	return (
		<div class="watch-ui__buffer">
			{/* Buffer Visualization - interactive, click/drag to set buffer */}
			<div
				class={`watch-ui__buffer-visualization ${isDragging() ? "watch-ui__buffer-visualization--dragging" : ""}`}
				ref={containerRef}
				onMouseDown={onMouseDown}
				onKeyDown={onKeyDown}
				role="slider"
				tabIndex={0}
				aria-valuenow={context.jitter()}
				aria-valuemin={MIN_RANGE}
				aria-valuemax={maxRange()}
				aria-label="Buffer jitter"
			>
				{/* Playhead (left edge = current time) */}
				<div class="watch-ui__buffer-playhead" />

				{/* Video buffer track */}
				<div class="watch-ui__buffer-track watch-ui__buffer-track--video">
					<span class="watch-ui__buffer-track-label">Video</span>
					<canvas ref={videoCanvasRef} class="watch-ui__buffer-canvas" />
				</div>

				{/* Audio buffer track */}
				<div class="watch-ui__buffer-track watch-ui__buffer-track--audio">
					<span class="watch-ui__buffer-track-label">Audio</span>
					<canvas ref={audioCanvasRef} class="watch-ui__buffer-canvas" />
				</div>

				{/* Buffer target line (draggable) - wrapped in track-area container */}
				<div class="watch-ui__buffer-target-area">
					<div class="watch-ui__buffer-target-line" style={{ left: `${bufferTargetPct()}%` }}>
						<span class="watch-ui__buffer-target-label">{`${Math.round(context.jitter())}ms`}</span>
					</div>
				</div>

				{/* Help text - disappears after first interaction */}
				{!hasInteracted() && <span class="watch-ui__buffer-help">click to change latency</span>}
			</div>
		</div>
	);
}

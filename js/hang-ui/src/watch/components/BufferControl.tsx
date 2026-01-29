import { createMemo, createSignal, For, Show } from "solid-js";
import useWatchUIContext from "../hooks/use-watch-ui";

const MIN_RANGE = 0;
const RANGE_STEP = 100;

type BufferControlProps = {
	/** Maximum buffer range in milliseconds (default: 5000ms = 5s) */
	max?: number;
};

export default function BufferControl(props: BufferControlProps) {
	const context = useWatchUIContext();
	const maxRange = () => props.max ?? 5000;
	const [isDragging, setIsDragging] = createSignal(false);

	// Compute range style and overflow info relative to current timestamp
	const computeRange = (range: { start: number; end: number }, timestamp: number, color: string) => {
		const startMs = (range.start - timestamp) * 1000;
		const endMs = (range.end - timestamp) * 1000;
		const visibleStartMs = Math.max(0, startMs);
		const visibleEndMs = Math.min(endMs, maxRange());
		const leftPct = (visibleStartMs / maxRange()) * 100;
		const widthPct = Math.max(0.5, ((visibleEndMs - visibleStartMs) / maxRange()) * 100);
		const isOverflow = endMs > maxRange();
		const overflowSec = isOverflow ? ((endMs - visibleStartMs) / 1000).toFixed(1) : null;
		return {
			style: `left: ${leftPct}%; width: ${widthPct}%; background: ${color};`,
			isOverflow,
			overflowSec,
		};
	};

	// Determine color based on gap detection and buffering state
	const rangeColor = (index: number, isBuffering: boolean) => {
		if (isBuffering) return "#f87171"; // red
		if (index > 0) return "#facc15"; // yellow
		return "#4ade80"; // green
	};

	const bufferTargetPct = createMemo(() => (context.delay() / maxRange()) * 100);

	// Handle mouse interaction to set buffer via clicking/dragging on the visualization
	let containerRef: HTMLDivElement | undefined;

	const LABEL_WIDTH = 48; // px reserved for track labels

	const updateBufferFromMouseX = (clientX: number) => {
		if (!containerRef) return;
		const rect = containerRef.getBoundingClientRect();
		const trackWidth = rect.width - LABEL_WIDTH;
		const x = Math.max(0, Math.min(clientX - rect.left - LABEL_WIDTH, trackWidth));
		const ms = (x / trackWidth) * maxRange();
		const snapped = Math.round(ms / RANGE_STEP) * RANGE_STEP;
		const clamped = Math.max(MIN_RANGE, Math.min(maxRange(), snapped));
		context.setDelay(clamped);
	};

	const onMouseDown = (e: MouseEvent) => {
		setIsDragging(true);
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

	return (
		<div class="bufferControlContainer">
			{/* Buffer Visualization - interactive, click/drag to set buffer */}
			<div
				class={`bufferVisualization ${isDragging() ? "dragging" : ""}`}
				ref={containerRef}
				onMouseDown={onMouseDown}
				role="slider"
				tabIndex={0}
			>
				{/* Playhead (left edge = current time) */}
				<div class="bufferPlayhead" />

				{/* Video buffer track */}
				<div class="bufferTrack bufferTrack--video">
					<span class="bufferTrackLabel">Video</span>
					<For each={context.videoBuffered()}>
						{(range, i) => {
							const info = () =>
								computeRange(range, context.timestamp(), rangeColor(i(), context.buffering()));
							return (
								<div class="bufferRange" style={info().style}>
									<Show when={info().isOverflow}>
										<span class="bufferOverflowLabel">{info().overflowSec}s</span>
									</Show>
								</div>
							);
						}}
					</For>
				</div>

				{/* Audio buffer track */}
				<div class="bufferTrack bufferTrack--audio">
					<span class="bufferTrackLabel">Audio</span>
					<For each={context.audioBuffered()}>
						{(range, i) => {
							const info = () =>
								computeRange(range, context.timestamp(), rangeColor(i(), context.buffering()));
							return (
								<div class="bufferRange" style={info().style}>
									<Show when={info().isOverflow}>
										<span class="bufferOverflowLabel">{info().overflowSec}s</span>
									</Show>
								</div>
							);
						}}
					</For>
				</div>

				{/* Buffer target line (draggable) - wrapped in track-area container */}
				<div class="bufferTargetArea">
					<div class="bufferTargetLine" style={{ left: `${bufferTargetPct()}%` }}>
						<span class="bufferTargetLabel">{`${Math.round(context.delay())}ms`}</span>
					</div>
				</div>
			</div>
		</div>
	);
}

import { Moq } from "@moq/hang";
import { createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import type { BufferedRange } from "../..";
import useWatchUIContext from "../hooks/use-watch-ui";

const MIN_RANGE = 0 as Moq.Time.Milli;
const RANGE_STEP = 100 as Moq.Time.Milli;

type BufferControlProps = {
	/** Maximum buffer range in milliseconds (default: 5000ms = 5s) */
	max?: Moq.Time.Milli;
};

export default function BufferControl(props: BufferControlProps) {
	const context = useWatchUIContext();
	const maxRange = (): Moq.Time.Milli => props.max ?? (5000 as Moq.Time.Milli);
	const [isDragging, setIsDragging] = createSignal(false);

	// Compute range style and overflow info relative to current timestamp
	const computeRange = (range: BufferedRange, timestamp: Moq.Time.Milli, color: string) => {
		const startMs = (range.start - timestamp) as Moq.Time.Milli;
		const endMs = (range.end - timestamp) as Moq.Time.Milli;
		const visibleStartMs = Math.max(0, startMs) as Moq.Time.Milli;
		const visibleEndMs = Math.min(endMs, maxRange()) as Moq.Time.Milli;
		const leftPct = (visibleStartMs / maxRange()) * 100;
		const widthPct = Math.max(0.5, ((visibleEndMs - visibleStartMs) / maxRange()) * 100);
		const isOverflow = endMs > maxRange();
		const overflowSec = isOverflow
			? Moq.Time.Milli.toSecond((endMs - visibleStartMs) as Moq.Time.Milli).toFixed(1)
			: null;
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

	const bufferTargetPct = createMemo(() => (context.jitter() / maxRange()) * 100);

	// Handle mouse interaction to set buffer via clicking/dragging on the visualization
	let containerRef: HTMLDivElement | undefined;

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

	// Cleanup listeners on unmount
	onCleanup(() => {
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
					<For each={context.videoBuffered()}>
						{(range, i) => {
							const info = () => {
								const timestamp = context.timestamp();
								if (timestamp === undefined) return null;
								return computeRange(range, timestamp, rangeColor(i(), context.buffering()));
							};
							return (
								<Show when={info()}>
									{(rangeInfo) => (
										<div class="watch-ui__buffer-range" style={rangeInfo().style}>
											<Show when={rangeInfo().isOverflow}>
												<span class="watch-ui__buffer-overflow-label">
													{rangeInfo().overflowSec}s
												</span>
											</Show>
										</div>
									)}
								</Show>
							);
						}}
					</For>
				</div>

				{/* Audio buffer track */}
				<div class="watch-ui__buffer-track watch-ui__buffer-track--audio">
					<span class="watch-ui__buffer-track-label">Audio</span>
					<For each={context.audioBuffered()}>
						{(range, i) => {
							const info = () => {
								const timestamp = context.timestamp();
								if (timestamp === undefined) return null;
								return computeRange(range, timestamp, rangeColor(i(), context.buffering()));
							};
							return (
								<Show when={info()}>
									{(rangeInfo) => (
										<div class="watch-ui__buffer-range" style={rangeInfo().style}>
											<Show when={rangeInfo().isOverflow}>
												<span class="watch-ui__buffer-overflow-label">
													{rangeInfo().overflowSec}s
												</span>
											</Show>
										</div>
									)}
								</Show>
							);
						}}
					</For>
				</div>

				{/* Buffer target line (draggable) - wrapped in track-area container */}
				<div class="watch-ui__buffer-target-area">
					<div class="watch-ui__buffer-target-line" style={{ left: `${bufferTargetPct()}%` }}>
						<span class="watch-ui__buffer-target-label">{`${Math.round(context.jitter())}ms`}</span>
					</div>
				</div>
			</div>
		</div>
	);
}

import { Show } from "solid-js/web";
import LatencySlider from "./LatencySlider";
import useWatchUIContext from "./useWatchUIContext";

/**
 * Overlay displaying buffer information for video and audio
 */
export default function BufferOverlay() {
	const context = useWatchUIContext();

	return (
		<Show when={context.isBufferOverlayVisible()}>
			<div class="bufferOverlay">
				<div class="bufferOverlayHeader">Buffer Stats</div>
				<div class="bufferOverlaySection">
					<div class="bufferOverlayLabel">Video</div>
					<div
						class="bufferOverlayValue"
						title={context.videoBufferDuration() === undefined ? "Video may be paused or not available" : undefined}
					>
						{context.videoBufferDuration() !== undefined
							? `${context.videoBufferDuration()?.toFixed(1)}ms buffered`
							: "No data"}
					</div>
				</div>
				<div class="bufferOverlaySection">
					<div class="bufferOverlayLabel">Audio</div>
					<div
						class="bufferOverlayValue"
						title={context.audioBufferDuration() === undefined ? "Audio may be muted or not available" : undefined}
					>
						{context.audioBufferDuration() !== undefined
							? `${context.audioBufferDuration()?.toFixed(1)}ms buffered`
							: "No data"}
					</div>
				</div>
				<div class="bufferOverlayLatency">
					<LatencySlider />
				</div>
			</div>
		</Show>
	);
}

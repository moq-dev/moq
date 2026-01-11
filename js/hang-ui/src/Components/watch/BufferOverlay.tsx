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
					<div class="bufferOverlayLabel">VIDEO</div>
					<div class="bufferOverlayValue">
						{context.videoBufferDuration() !== undefined
							? `${context.videoBufferDuration()?.toFixed(1)}ms buffered`
							: "No data"}
					</div>
				</div>
				<div class="bufferOverlaySection">
					<div class="bufferOverlayLabel">AUDIO</div>
					<div class="bufferOverlayValue">
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

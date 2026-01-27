import { Show } from "solid-js/web";
import LatencySlider from "./LatencySlider";
import BufferTimeline from "./BufferTimeline";
import useWatchUIContext from "../hooks/use-watch-ui";

/**
 * Overlay displaying buffer information for video and audio
 */
export default function BufferOverlay() {
	const context = useWatchUIContext();

	return (
		<Show when={context.isBufferOverlayVisible()}>
			<div class="bufferOverlay">
				<div class="bufferOverlayHeader">Buffer Information</div>
				<div class="bufferOverlaySection">
					<div class="bufferOverlayLabel">Video</div>
					<BufferTimeline
						ranges={context.videoBufferedRanges()}
						currentTime={context.videoCurrentTime()}
						latency={context.latency()}
					/>
				</div>
				<div class="bufferOverlaySection">
					<div class="bufferOverlayLabel">Audio</div>
					<BufferTimeline
						ranges={context.audioBufferedRanges()}
						currentTime={context.videoCurrentTime()}
						latency={context.latency()}
						isPlaying={!context.isMuted()}
					/>
				</div>
				<div class="bufferOverlayLatency">
					<LatencySlider />
				</div>
			</div>
		</Show>
	);
}

import { For, Show } from "solid-js/web";
import LatencySlider from "./LatencySlider";
import useWatchUIContext from "./useWatchUIContext";

type BufferDetails = {
	content: "Video" | "Audio";
	duration: number | undefined;
	noDataMessage: string;
};

/**
 * Overlay displaying buffer information for video and audio
 */
export default function BufferOverlay() {
	const context = useWatchUIContext();
	const bufferDetails = (): BufferDetails[] => [{
		content: "Video",
		duration: context.videoBufferDuration(),
		noDataMessage: "Video may be paused or not available",
	}, {
		content: "Audio",
		duration: context.audioBufferDuration(),
		noDataMessage: "Audio may be muted or not available",
	}];

	return (
		<Show when={context.isBufferOverlayVisible()}>
			<div class="bufferOverlay">
				<div class="bufferOverlayHeader">Buffer Stats</div>
				<For each={bufferDetails()}>
					{
						(section) => (
							<div class="bufferOverlaySection">
								<div class="bufferOverlayLabel">{section.content}</div>
								<div
									class="bufferOverlayValue"
									title={
										section.duration === undefined
											? section.noDataMessage
											: undefined
									}
								>
									{section.duration !== undefined
										? `${section.duration?.toFixed(1)}ms buffered`
										: "No data"}
								</div>
							</div>
						)
					}
				</For>
				<div class="bufferOverlayLatency">
					<LatencySlider />
				</div>
			</div>
		</Show>
	);
}

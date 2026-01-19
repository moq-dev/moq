import BufferButton from "./BufferButton";
import FullscreenButton from "./FullscreenButton";
import PlayPauseButton from "./PlayPauseButton";
import QualitySelector from "./QualitySelector";
import StatsButton from "./StatsButton";
import VolumeSlider from "./VolumeSlider";
import WatchStatusIndicator from "./WatchStatusIndicator";

export default function WatchControls() {
	return (
		<div class="watchControlsContainer">
			<div class="playbackControlsRow flex--align-center">
				<PlayPauseButton />
				<VolumeSlider />
				<WatchStatusIndicator />
				<BufferButton />
				<StatsButton />
				<FullscreenButton />
			</div>
			<div class="latencyControlsRow">
				<QualitySelector />
			</div>
		</div>
	);
}

import BufferControl from "./BufferControl";
import FullscreenButton from "./FullscreenButton";
import PlayPauseButton from "./PlayPauseButton";
import QualitySelector from "./QualitySelector";
import StatsButton from "./StatsButton";
import VolumeSlider from "./VolumeSlider";
import WatchStatusIndicator from "./WatchStatusIndicator";

export default function WatchControls() {
	return (
		<div class="watch-ui__controls">
			<div class="watch-ui__playback-controls flex--align-center">
				<PlayPauseButton />
				<VolumeSlider />
				<WatchStatusIndicator />
				<StatsButton />
				<FullscreenButton />
			</div>
			<div class="watch-ui__latency-controls">
				<BufferControl />
				<QualitySelector />
			</div>
		</div>
	);
}

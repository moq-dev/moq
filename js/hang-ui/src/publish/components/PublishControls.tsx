import CameraSourceButton from "./CameraSourceButton";
import FileSourceButton from "./FileSourceButton";
import MicrophoneSourceButton from "./MicrophoneSourceButton";
import NothingSourceButton from "./NothingSourceButton";
import PublishStatusIndicator from "./PublishStatusIndicator";
import ScreenSourceButton from "./ScreenSourceButton";

export default function PublishControls() {
	return (
		<div class="publishControlsContainer">
			<div class="publishSourceSelectorContainer">
				Source:
				<MicrophoneSourceButton />
				<CameraSourceButton />
				<ScreenSourceButton />
				<FileSourceButton />
				<NothingSourceButton />
			</div>
			<PublishStatusIndicator />
		</div>
	);
}

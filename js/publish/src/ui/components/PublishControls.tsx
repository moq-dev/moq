import CameraSourceButton from "./CameraSourceButton";
import FileSourceButton from "./FileSourceButton";
import MicrophoneSourceButton from "./MicrophoneSourceButton";
import NothingSourceButton from "./NothingSourceButton";
import PublishStatusIndicator from "./PublishStatusIndicator";
import ScreenSourceButton from "./ScreenSourceButton";

export default function PublishControls() {
	return (
		<div class="publish-ui__controls flex--center flex--space-between">
			<div class="publish-ui__source-selector flex--center">
				<span class="publish-ui__source-label">Source:</span>
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

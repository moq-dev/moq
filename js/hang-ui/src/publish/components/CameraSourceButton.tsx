import { Show } from "solid-js";
import Button from "../../shared/components/button/button";
import * as Icon from "../../shared/components/icon/icon";
import usePublishUIContext from "../hooks/use-publish-ui";
import MediaSourceSourceSelector from "./MediaSourceSelector";

export default function CameraSourceButton() {
	const context = usePublishUIContext();
	const onClick = () => {
		if (context.hangPublish.source.peek() === "camera") {
			// Camera already selected, toggle video.
			context.hangPublish.invisible.update((invisible) => !invisible);
		} else {
			context.hangPublish.source.set("camera");
			context.hangPublish.invisible.set(false);
		}
	};

	const onSourceSelected = (sourceId: MediaDeviceInfo["deviceId"]) => {
		const video = context.hangPublish.video.peek();
		if (!video || !("device" in video)) return;

		video.device.preferred.set(sourceId);
	};

	return (
		<div class="publishSourceButtonContainer">
			<Button
				title="Camera"
				class={`publishSourceButton ${context.cameraActive() ? "active" : ""}`}
				onClick={onClick}
			>
				<Icon.Camera />
			</Button>
			<Show when={context.cameraActive() && context.cameraDevices().length}>
				<MediaSourceSourceSelector
					sources={context.cameraDevices()}
					selectedSource={context.selectedCameraSource?.()}
					onSelected={onSourceSelected}
				/>
			</Show>
		</div>
	);
}

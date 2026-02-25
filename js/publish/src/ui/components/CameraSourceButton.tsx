import { Button, Icon } from "@moq/ui-core";
import { Show } from "solid-js";
import usePublishUIContext from "../hooks/use-publish-ui";
import MediaSourceSourceSelector from "./MediaSourceSelector";

export default function CameraSourceButton() {
	const context = usePublishUIContext();
	const onClick = () => {
		if (context.moqPublish.source === "camera") {
			// Camera already selected, toggle video.
			context.moqPublish.invisible = !context.moqPublish.invisible;
		} else {
			context.moqPublish.source = "camera";
			context.moqPublish.invisible = false;
		}
	};

	const onSourceSelected = (sourceId: MediaDeviceInfo["deviceId"]) => {
		const video = context.moqPublish.video.peek();
		if (!video || !("device" in video)) return;

		video.device.preferred.set(sourceId);
	};

	return (
		<div class="publish-ui__source-button-wrapper flex--center">
			<Button
				title="Camera"
				class={`publish-ui__source-button flex--center ${context.cameraActive() ? "publish-ui__source-button--active" : ""}`}
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

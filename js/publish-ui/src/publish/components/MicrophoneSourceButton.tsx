import { Show } from "solid-js";
import Button from "../../shared/components/button/button";
import * as Icon from "../../shared/components/icon/icon";
import usePublishUIContext from "../hooks/use-publish-ui";
import MediaSourceSourceSelector from "./MediaSourceSelector";

export default function MicrophoneSourceButton() {
	const context = usePublishUIContext();
	const onClick = () => {
		if (context.hangPublish.source.peek() === "camera") {
			// Camera already selected, toggle audio.
			context.hangPublish.muted.update((muted) => !muted);
		} else {
			context.hangPublish.source.set("camera");
			context.hangPublish.muted.set(false);
		}
	};

	const onSourceSelected = (sourceId: MediaDeviceInfo["deviceId"]) => {
		const audio = context.hangPublish.audio.peek();
		if (!audio || !("device" in audio)) return;

		audio.device.preferred.set(sourceId);
	};

	return (
		<div class="publish-ui__source-button-wrapper flex--center">
			<Button
				title="Microphone"
				class={`publish-ui__source-button flex--center ${context.microphoneActive() ? "publish-ui__source-button--active" : ""}`}
				onClick={onClick}
			>
				<Icon.Microphone />
			</Button>
			<Show when={context.microphoneActive() && context.microphoneDevices().length}>
				<MediaSourceSourceSelector
					sources={context.microphoneDevices()}
					selectedSource={context.selectedMicrophoneSource?.()}
					onSelected={onSourceSelected}
				/>
			</Show>
		</div>
	);
}

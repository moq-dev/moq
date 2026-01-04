import { Show } from "solid-js";
import Button from "../shared/components/button";
import Icon from "../shared/components/icon";
import MediaSourceSourceSelector from "./MediaSourceSelector";
import usePublishUIContext from "./usePublishUIContext";

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
		<div class="publishSourceButtonContainer">
			<Button
				title="Microphone"
				class={`publishSourceButton ${context.microphoneActive() ? "active" : ""}`}
				onClick={onClick}
			>
				<Icon name="microphone" />
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

import { Show } from "solid-js";
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
			<button
				type="button"
				title="Microphone"
				class={`publishButton publishSourceButton ${context.microphoneActive() ? "active" : ""}`}
				onClick={onClick}
			>
				🎤
			</button>
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

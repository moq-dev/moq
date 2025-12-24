import { Show } from "solid-js";
import Button from "../shared/components/button";
import type { Icon } from "../shared/types/icons";
import MediaSourceSourceSelector from "./MediaSourceSelector";
import usePublishUIContext from "./usePublishUIContext";

const microphoneIcon: Icon = () => (
	<svg
		xmlns="http://www.w3.org/2000/svg"
		width="24"
		height="24"
		viewBox="0 0 24 24"
		fill="none"
		stroke="currentColor"
		stroke-width="2"
		stroke-linecap="round"
		stroke-linejoin="round"
		aria-hidden="true"
	>
		<path d="M12 19v3" />
		<path d="M19 10v2a7 7 0 0 1-14 0v-2" />
		<rect x="9" y="2" width="6" height="13" rx="3" />
	</svg>
);

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
				{microphoneIcon()}
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

import { Show } from "solid-js";
import Button from "../shared/components/button";
import type { Icon } from "../shared/types/icons";
import MediaSourceSourceSelector from "./MediaSourceSelector";
import usePublishUIContext from "./usePublishUIContext";

const cameraIcon: Icon = () => (
	<svg
		xmlns="http://www.w3.org/2000/svg"
		width="24"
		height="24"
		viewBox="0 0 24 24"
		fill="none"
		stroke="var(--color-white)"
		stroke-width="2"
		stroke-linecap="round"
		stroke-linejoin="round"
		aria-hidden="true"
	>
		<path d="M13.997 4a2 2 0 0 1 1.76 1.05l.486.9A2 2 0 0 0 18.003 7H20a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V9a2 2 0 0 1 2-2h1.997a2 2 0 0 0 1.759-1.048l.489-.904A2 2 0 0 1 10.004 4z" />
		<circle cx="12" cy="13" r="3" />
	</svg>
);

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
				{cameraIcon()}
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

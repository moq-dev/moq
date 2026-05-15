import { Effect } from "@moq/signals";
import type MoqPublish from "../../element";
import { camera, icon } from "../icons";
import { mediaSourceSelector } from "./media-source-selector";

export function cameraSourceButton(parent: Effect, publish: MoqPublish): HTMLElement {
	const wrapper = document.createElement("div");
	wrapper.className = "publish-ui__source-button-wrapper flex--center";

	const button = document.createElement("button");
	button.type = "button";
	button.title = "Camera";
	button.setAttribute("aria-label", "Camera");
	button.appendChild(icon(camera));
	wrapper.appendChild(button);

	parent.event(button, "click", () => {
		if (publish.source === "camera") {
			publish.invisible = !publish.invisible;
		} else {
			publish.source = "camera";
			publish.invisible = false;
		}
	});

	parent.run((effect) => {
		const source = effect.get(publish.state.source);
		const invisible = effect.get(publish.state.invisible);
		const active = source === "camera" && !invisible;
		button.className = `button publish-ui__source-button flex--center${active ? " publish-ui__source-button--active" : ""}`;

		// Tear down any existing selector before deciding whether to render a new one.
		const video = active ? effect.get(publish.video) : undefined;
		if (!video || !("device" in video)) return;

		const devices = effect.get(video.device.available);
		if (!devices || devices.length < 2) return;

		const inner = new Effect();
		effect.cleanup(() => inner.close());
		const selector = mediaSourceSelector(inner, {
			getDevices: () => devices,
			getSelected: () => video.device.requested.peek(),
			onSelected: (id) => video.device.preferred.set(id),
		});
		wrapper.appendChild(selector);
		inner.cleanup(() => selector.remove());
	});

	return wrapper;
}

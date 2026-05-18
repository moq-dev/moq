import type { Effect } from "@moq/signals";
import * as DOM from "@moq/signals/dom";
import type MoqPublish from "../../element";
import { camera, icon } from "../icons";
import { mediaSourceSelector } from "./media-source-selector";

export function cameraSourceButton(parent: Effect, publish: MoqPublish): HTMLElement {
	const wrapper = DOM.create("div", { className: "source-button-wrapper flex-center" });

	const button = DOM.create("button", {
		type: "button",
		title: "Camera",
	});
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
		button.className = `button source-button flex-center${active ? " source-button--active" : ""}`;

		if (!active) return;

		const video = effect.get(publish.video);
		if (!video || !("device" in video)) return;

		const devices = effect.get(video.device.available);
		if (!devices || devices.length < 2) return;

		DOM.render(
			effect,
			wrapper,
			mediaSourceSelector(effect, {
				getDevices: () => devices,
				getSelected: () => video.device.requested.peek(),
				onSelected: (id) => video.device.preferred.set(id),
			}),
		);
	});

	return wrapper;
}

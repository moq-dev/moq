import type { Effect } from "@moq/signals";
import * as DOM from "@moq/signals/dom";
import type MoqPublish from "../../element";
import { icon, microphone } from "../icons";
import { mediaSourceSelector } from "./media-source-selector";

export function microphoneSourceButton(parent: Effect, publish: MoqPublish): HTMLElement {
	const wrapper = DOM.create("div", { className: "publish-ui__source-button-wrapper flex--center" });

	const button = DOM.create("button", {
		type: "button",
		title: "Microphone",
	});
	button.setAttribute("aria-label", "Microphone");
	button.appendChild(icon(microphone));
	wrapper.appendChild(button);

	parent.event(button, "click", () => {
		if (publish.source === "camera") {
			publish.muted = !publish.muted;
		} else {
			publish.source = "camera";
			publish.muted = false;
		}
	});

	parent.run((effect) => {
		const muted = effect.get(publish.state.muted);
		const active = !muted;
		button.className = `button publish-ui__source-button flex--center${active ? " publish-ui__source-button--active" : ""}`;

		if (!active) return;

		const audio = effect.get(publish.audio);
		if (!audio || !("device" in audio)) return;

		const enabled = effect.get(publish.broadcast.audio.enabled);
		if (!enabled) return;

		const devices = effect.get(audio.device.available);
		if (!devices || devices.length < 2) return;

		DOM.render(
			effect,
			wrapper,
			mediaSourceSelector(effect, {
				getDevices: () => devices,
				getSelected: () => audio.device.requested.peek(),
				onSelected: (id) => audio.device.preferred.set(id),
			}),
		);
	});

	return wrapper;
}

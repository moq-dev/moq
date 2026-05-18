import type { Effect } from "@moq/signals";
import type MoqWatch from "../../element";
import { icon, mute, volumeHigh, volumeLow, volumeMedium } from "../icons";

function volumeIcon(volumePct: number, muted: boolean): string {
	if (muted || volumePct === 0) return mute;
	if (volumePct <= 33) return volumeLow;
	if (volumePct <= 66) return volumeMedium;
	return volumeHigh;
}

export function volumeSlider(parent: Effect, watch: MoqWatch): HTMLElement {
	const wrapper = document.createElement("div");
	wrapper.className = "volume-slider flex-center";

	const button = document.createElement("button");
	button.type = "button";
	button.className = "button flex-center";

	const slider = document.createElement("input");
	slider.type = "range";
	slider.min = "0";
	slider.max = "100";

	const label = document.createElement("span");
	label.className = "volume-label";

	wrapper.append(button, slider, label);

	parent.run((effect) => {
		const volume = effect.get(watch.backend.audio.volume);
		const muted = effect.get(watch.backend.audio.muted);
		const pct = Math.round(volume * 100);
		slider.value = pct.toString();
		label.textContent = pct.toString();
		button.title = muted ? "Unmute" : "Mute";
		button.setAttribute("aria-label", button.title);
		button.replaceChildren(icon(volumeIcon(pct, muted)));
	});

	parent.event(slider, "input", () => {
		watch.backend.audio.volume.set(Number.parseFloat(slider.value) / 100);
	});

	parent.event(button, "click", () => {
		watch.backend.audio.muted.update((m) => !m);
	});

	return wrapper;
}

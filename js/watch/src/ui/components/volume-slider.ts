import type { Effect } from "@moq/signals";
import type MoqWatch from "../../element";
import { icon, mute, volumeHigh, volumeLow, volumeMedium } from "../icons";

function volumeIcon(volumePct: number, muted: boolean): string {
	if (muted || volumePct === 0) return mute;
	if (volumePct <= 33) return volumeLow;
	if (volumePct <= 66) return volumeMedium;
	return volumeHigh;
}

/** Volume control with a mute toggle; the slider expands on hover. */
export function volumeSlider(parent: Effect, watch: MoqWatch): HTMLElement {
	const wrapper = document.createElement("div");
	wrapper.className = "volume flex-align-center";

	const button = document.createElement("button");
	button.type = "button";
	button.className = "control flex-center";

	const slider = document.createElement("input");
	slider.type = "range";
	slider.className = "volume-track";
	slider.min = "0";
	slider.max = "100";
	slider.setAttribute("aria-label", "Volume");

	wrapper.append(button, slider);

	parent.run((effect) => {
		const volume = effect.get(watch.backend.audio.volume);
		const muted = effect.get(watch.backend.audio.muted);
		const pct = Math.round(volume * 100);
		const shown = muted ? 0 : pct;
		slider.value = shown.toString();
		// Drive the filled portion of the track via a CSS variable.
		slider.style.setProperty("--fill", `${shown}%`);
		button.title = muted ? "Unmute" : "Mute";
		button.setAttribute("aria-label", button.title);
		button.replaceChildren(icon(volumeIcon(pct, muted)));
	});

	parent.event(slider, "input", () => {
		const value = Number.parseFloat(slider.value) / 100;
		watch.backend.audio.volume.set(value);
		// Any non-zero adjustment implies an intent to hear audio.
		if (value > 0) watch.backend.audio.muted.set(false);
	});

	parent.event(button, "click", () => {
		watch.backend.audio.muted.update((m) => !m);
	});

	return wrapper;
}

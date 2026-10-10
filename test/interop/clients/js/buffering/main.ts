/**
 * Real watch controls with deterministic player state, so a stall needs no relay or wall clock.
 * @module
 */
import { Effect, Signal } from "@moq/signals";
import type MoqWatch from "../../../../../js/watch/src/element";
import { bufferingIndicator } from "../../../../../js/watch/src/ui/components/buffering-indicator";
import { centerPlay } from "../../../../../js/watch/src/ui/components/center-play";
import { playPauseButton } from "../../../../../js/watch/src/ui/components/play-pause";
import styles from "../../../../../js/watch/src/ui/styles/index.css?inline";

const paused = new Signal(false);
const video = new URL(location.href).searchParams.has("video");
// Only the dependencies read by these three components are needed. Own the signals here so
// the test can hold either stall indefinitely without audio devices, networking, or timers.
const watch = {
	controls: { paused, delay: new Signal(100) },
	audio: {
		in: { enabled: new Signal(true) },
		source: { out: { config: new Signal({ codec: "opus" }) } },
		out: { stalled: new Signal(!video) },
	},
	video: {
		source: { out: { error: new Signal(undefined) } },
		out: { stalled: new Signal(video) },
	},
	broadcast: { out: { status: new Signal("live"), error: new Signal(undefined) } },
	get paused() {
		return paused.peek();
	},
	set paused(value: boolean) {
		paused.set(value);
	},
} as unknown as MoqWatch;

const root = document.body.attachShadow({ mode: "open" });
const style = document.createElement("style");
style.textContent = styles;
const player = document.createElement("div");
player.className = "player";
player.style.cssText = "width: 640px; height: 360px";
const center = document.createElement("div");
center.className = "center";
const effect = new Effect();
center.append(centerPlay(effect, watch), bufferingIndicator(effect, watch));
player.append(center);
root.append(style, player, playPauseButton(effect, watch));
new Effect((effect) => {
	document.body.dataset.paused = String(effect.get(paused));
});

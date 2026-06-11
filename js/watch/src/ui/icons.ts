import audio from "./icons/audio.svg?raw";
import buffer from "./icons/buffer.svg?raw";
import check from "./icons/check.svg?raw";
import close from "./icons/close.svg?raw";
import fullscreenEnter from "./icons/fullscreen-enter.svg?raw";
import fullscreenExit from "./icons/fullscreen-exit.svg?raw";
import gauge from "./icons/gauge.svg?raw";
import mute from "./icons/mute.svg?raw";
import network from "./icons/network.svg?raw";
import pause from "./icons/pause.svg?raw";
import play from "./icons/play.svg?raw";
import settings from "./icons/settings.svg?raw";
import stats from "./icons/stats.svg?raw";
import video from "./icons/video.svg?raw";
import volumeHigh from "./icons/volume-high.svg?raw";
import volumeLow from "./icons/volume-low.svg?raw";
import volumeMedium from "./icons/volume-medium.svg?raw";

export {
	audio,
	buffer,
	check,
	close,
	fullscreenEnter,
	fullscreenExit,
	gauge,
	mute,
	network,
	pause,
	play,
	settings,
	stats,
	video,
	volumeHigh,
	volumeLow,
	volumeMedium,
};

export function icon(svg: string): HTMLElement {
	const span = document.createElement("span");
	span.className = "flex-center";
	span.setAttribute("role", "img");
	span.setAttribute("aria-hidden", "true");
	span.innerHTML = svg;
	return span;
}

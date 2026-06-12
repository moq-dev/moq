import audio from "./icons/audio.svg?raw";
import ban from "./icons/ban.svg?raw";
import camera from "./icons/camera.svg?raw";
import cameraOff from "./icons/camera-off.svg?raw";
import check from "./icons/check.svg?raw";
import close from "./icons/close.svg?raw";
import file from "./icons/file.svg?raw";
import fullscreenEnter from "./icons/fullscreen-enter.svg?raw";
import fullscreenExit from "./icons/fullscreen-exit.svg?raw";
import micOff from "./icons/mic-off.svg?raw";
import microphone from "./icons/microphone.svg?raw";
import screen from "./icons/screen.svg?raw";
import settings from "./icons/settings.svg?raw";
import video from "./icons/video.svg?raw";
import wifi from "./icons/wifi.svg?raw";

export {
	audio,
	ban,
	camera,
	cameraOff,
	check,
	close,
	file,
	fullscreenEnter,
	fullscreenExit,
	micOff,
	microphone,
	screen,
	settings,
	video,
	wifi,
};

export function icon(svg: string): HTMLElement {
	const span = document.createElement("span");
	span.className = "flex-center";
	span.setAttribute("role", "img");
	span.setAttribute("aria-hidden", "true");
	span.innerHTML = svg;
	return span;
}

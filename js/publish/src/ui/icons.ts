import arrowDown from "./icons/arrow-down.svg?raw";
import arrowUp from "./icons/arrow-up.svg?raw";
import ban from "./icons/ban.svg?raw";
import camera from "./icons/camera.svg?raw";
import file from "./icons/file.svg?raw";
import microphone from "./icons/microphone.svg?raw";
import screen from "./icons/screen.svg?raw";

export { arrowDown, arrowUp, ban, camera, file, microphone, screen };

export function icon(svg: string): HTMLElement {
	const span = document.createElement("span");
	span.className = "flex-center";
	span.setAttribute("role", "img");
	span.setAttribute("aria-hidden", "true");
	span.innerHTML = svg;
	return span;
}

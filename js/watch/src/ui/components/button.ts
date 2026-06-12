import { icon } from "../icons";

/** A round overlay control button with an SVG glyph. */
export function controlButton(svg: string, label: string): HTMLButtonElement {
	const button = document.createElement("button");
	button.type = "button";
	button.className = "control flex-center";
	button.title = label;
	button.setAttribute("aria-label", label);
	button.replaceChildren(icon(svg));
	return button;
}

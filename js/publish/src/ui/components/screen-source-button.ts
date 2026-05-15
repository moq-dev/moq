import type { Effect } from "@moq/signals";
import type MoqPublish from "../../element";
import { icon, screen } from "../icons";

export function screenSourceButton(parent: Effect, publish: MoqPublish): HTMLElement {
	const button = document.createElement("button");
	button.type = "button";
	button.title = "Screen";
	button.setAttribute("aria-label", "Screen");
	button.appendChild(icon(screen));

	parent.run((effect) => {
		const source = effect.get(publish.state.source);
		const invisible = effect.get(publish.state.invisible);
		const active = source === "screen" && !invisible;
		button.className = `button publish-ui__source-button flex--center${active ? " publish-ui__source-button--active" : ""}`;
	});

	parent.event(button, "click", () => {
		publish.source = "screen";
		publish.invisible = false;
		publish.muted = true;
	});

	return button;
}

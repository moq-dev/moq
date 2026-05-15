import type { Effect } from "@moq/signals";
import type MoqPublish from "../../element";
import { ban, icon } from "../icons";

export function nothingSourceButton(parent: Effect, publish: MoqPublish): HTMLElement {
	const button = document.createElement("button");
	button.type = "button";
	button.title = "No Source";
	button.setAttribute("aria-label", "No Source");
	button.appendChild(icon(ban));

	parent.run((effect) => {
		const source = effect.get(publish.state.source);
		const muted = effect.get(publish.state.muted);
		const invisible = effect.get(publish.state.invisible);
		const active = source === undefined && muted && invisible;
		button.className = `button publish-ui__source-button flex--center publish-ui__source-button--no-source${active ? " publish-ui__source-button--no-source-active" : ""}`;
	});

	parent.event(button, "click", () => {
		publish.source = undefined;
		publish.muted = true;
		publish.invisible = true;
	});

	return button;
}

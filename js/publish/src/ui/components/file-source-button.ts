import type { Effect } from "@moq/signals";
import type MoqPublish from "../../element";
import { file, icon } from "../icons";

export function fileSourceButton(parent: Effect, publish: MoqPublish): HTMLElement {
	const input = document.createElement("input");
	input.type = "file";
	input.accept = "video/*,audio/*,image/*";
	input.style.display = "none";

	const button = document.createElement("button");
	button.type = "button";
	button.title = "Upload File";
	button.setAttribute("aria-label", "Upload File");
	button.appendChild(icon(file));

	const wrapper = document.createElement("span");
	wrapper.append(input, button);

	parent.run((effect) => {
		const source = effect.get(publish.state.source);
		const active = source instanceof File;
		button.className = `button source-button flex-center${active ? " source-button--active" : ""}`;
	});

	parent.event(button, "click", () => input.click());

	parent.event(input, "change", () => {
		const f = input.files?.[0];
		if (!f) return;
		publish.source = f;
		publish.invisible = false;
		publish.muted = true;
		input.value = "";
	});

	return wrapper;
}

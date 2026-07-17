import type { Effect } from "@moq/signals";
import * as DOM from "@moq/signals/dom";
import type MoqPublish from "../../element";
import type { SourceType } from "../../element";
import { camera, cameraOff, file, icon, screen } from "../icons";

const OPTIONS: { id: SourceType; label: string; svg: string }[] = [
	{ id: "camera", label: "Camera", svg: camera },
	{ id: "screen", label: "Screen", svg: screen },
	{ id: "file", label: "File", svg: file },
];

function pick(publish: MoqPublish, source: SourceType) {
	publish.source = source;
	publish.invisible = false;
	// Screen capture rarely wants the microphone; the others do.
	publish.muted = source === "screen";
}

function sourcePicker(parent: Effect, publish: MoqPublish): HTMLElement {
	const picker = DOM.create("div", { className: "picker" });
	picker.appendChild(DOM.create("div", { className: "picker-title" }, "Choose a source to go live"));

	const options = DOM.create("div", { className: "picker-options" });
	for (const opt of OPTIONS) {
		const button = DOM.create("button", { className: "picker-option", type: "button" });
		button.append(icon(opt.svg), DOM.create("span", {}, opt.label));
		parent.event(button, "click", () => pick(publish, opt.id));
		options.appendChild(button);
	}
	picker.appendChild(options);
	return picker;
}

function placeholder(): HTMLElement {
	const el = DOM.create("div", { className: "placeholder" });
	el.append(icon(cameraOff), DOM.create("span", {}, "Video off"));
	return el;
}

/** Center overlay: source picker when idle, "video off" notice when hidden. */
export function center(parent: Effect, publish: MoqPublish): HTMLElement {
	const container = DOM.create("div", { className: "center" });

	parent.run((effect) => {
		const source = effect.get(publish.controls.source);
		const invisible = effect.get(publish.controls.invisible);

		if (source === undefined) {
			DOM.render(effect, container, sourcePicker(effect, publish));
		} else if (invisible) {
			DOM.render(effect, container, placeholder());
		}
	});

	return container;
}

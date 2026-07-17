import type { Effect } from "@moq/signals";
import * as DOM from "@moq/signals/dom";
import type MoqPublish from "../../element";
import type { SourceType } from "../../element";
import type * as Source from "../../source";
import { ban, camera, file, icon, microphone, screen } from "../icons";

type SourceValue = SourceType | undefined;

const OPTIONS: { id: SourceValue; label: string; svg: string }[] = [
	{ id: "camera", label: "Camera", svg: camera },
	{ id: "screen", label: "Screen", svg: screen },
	{ id: "file", label: "File", svg: file },
	{ id: undefined, label: "Off", svg: ban },
];

function select(publish: MoqPublish, id: SourceValue) {
	if (id === undefined) {
		publish.source = undefined;
		publish.muted = true;
		publish.invisible = true;
		return;
	}
	publish.source = id;
	publish.invisible = false;
	publish.muted = id === "screen";
}

function sourceGrid(parent: Effect, publish: MoqPublish): HTMLElement {
	const grid = DOM.create("div", { className: "source-grid" });
	const buttons = OPTIONS.map((opt) => {
		const button = DOM.create("button", { className: "source-opt", type: "button" });
		button.append(icon(opt.svg), DOM.create("span", {}, opt.label));
		parent.event(button, "click", () => select(publish, opt.id));
		grid.appendChild(button);
		return { opt, button };
	});

	parent.run((effect) => {
		const active = effect.get(publish.controls.source);
		for (const { opt, button } of buttons) {
			button.classList.toggle("source-opt--active", opt.id === active);
		}
	});

	return grid;
}

function deviceField(
	parent: Effect,
	label: string,
	svg: string,
	device: Source.Device<"audio" | "video">,
): HTMLElement {
	const field = DOM.create("div", { className: "device-field" });
	const labelEl = DOM.create("div", { className: "device-field-label" });
	labelEl.append(icon(svg), DOM.create("span", {}, label));
	const dropdown = DOM.create("select", { className: "device-select" });
	field.append(labelEl, dropdown);

	parent.run((effect) => {
		const devices = effect.get(device.out.available) ?? [];
		const selected = effect.get(device.out.requested);
		dropdown.replaceChildren();

		if (devices.length === 0) {
			dropdown.appendChild(DOM.create("option", {}, "No devices"));
			dropdown.disabled = true;
			return;
		}

		dropdown.disabled = false;
		for (const d of devices) {
			const option = DOM.create("option", { value: d.deviceId }, d.label || "Unknown device");
			dropdown.appendChild(option);
		}
		if (selected) dropdown.value = selected;
	});

	parent.event(dropdown, "change", () => device.preferred.set(dropdown.value));
	return field;
}

/** The Source tab: pick a capture source and its devices. */
export function sourceTab(parent: Effect, publish: MoqPublish): HTMLElement {
	const container = DOM.create("div", { className: "tab-body" });
	container.appendChild(sourceGrid(parent, publish));

	// Device selection only applies to the camera source.
	const devices = DOM.create("div");
	parent.run((effect) => {
		if (effect.get(publish.controls.source) !== "camera") return;

		const video = effect.get(publish.sources.video);
		const audio = effect.get(publish.sources.audio);
		const fields: HTMLElement[] = [];
		if (video && "device" in video) fields.push(deviceField(effect, "Camera", camera, video.device));
		if (audio && "device" in audio) fields.push(deviceField(effect, "Microphone", microphone, audio.device));
		if (fields.length === 0) return;

		const section = DOM.create("div");
		section.appendChild(DOM.create("div", { className: "tab-section-label" }, "Devices"));
		for (const f of fields) section.appendChild(f);
		DOM.render(effect, devices, section);
	});

	// File selection only applies to the file source.
	const filePicker = DOM.create("div");
	parent.run((effect) => {
		const source = effect.get(publish.controls.source);
		if (source !== "file" && !(source instanceof File)) return;

		const fileSource = effect.get(publish.sources.file);
		if (!fileSource) return;

		DOM.render(effect, filePicker, fileField(effect, fileSource));
	});

	container.appendChild(filePicker);
	return container;
}

function fileField(parent: Effect, source: Source.File): HTMLElement {
	const section = DOM.create("div");
	section.appendChild(DOM.create("div", { className: "tab-section-label" }, "File"));

	const field = DOM.create("div", { className: "device-field" });
	const labelEl = DOM.create("div", { className: "device-field-label" });
	const name = DOM.create("span", {}, "No file");
	labelEl.append(icon(file), name);

	const button = DOM.create("button", { className: "source-opt", type: "button" }, "Choose");
	field.append(labelEl, button);
	section.appendChild(field);

	parent.run((effect) => {
		const picked = effect.get(source.file);
		name.textContent = picked?.name ?? "No file";
	});

	// A click is a user gesture, so the picker can open synchronously.
	parent.event(button, "click", () => source.prompt());

	return section;
}

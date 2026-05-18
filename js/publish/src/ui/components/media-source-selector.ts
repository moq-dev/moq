import type { Effect } from "@moq/signals";
import { arrowDown, arrowUp, icon } from "../icons";

export type SelectorOptions = {
	getDevices: () => MediaDeviceInfo[];
	getSelected: () => MediaDeviceInfo["deviceId"] | undefined;
	onSelected: (id: MediaDeviceInfo["deviceId"]) => void;
};

export function mediaSourceSelector(parent: Effect, opts: SelectorOptions): HTMLElement {
	const wrapper = document.createElement("div");
	wrapper.className = "media-selector-wrapper flex-center";

	const toggle = document.createElement("button");
	toggle.type = "button";
	toggle.className = "button media-selector-toggle";
	toggle.title = "Show Sources";
	toggle.appendChild(icon(arrowDown));

	const select = document.createElement("select");
	select.className = "media-selector-dropdown";
	select.style.display = "none";

	wrapper.append(toggle, select);

	let visible = false;
	const render = () => {
		toggle.replaceChildren(icon(visible ? arrowUp : arrowDown));
		toggle.title = visible ? "Hide Sources" : "Show Sources";
		select.style.display = visible ? "" : "none";

		const devices = opts.getDevices();
		select.replaceChildren();
		for (const device of devices) {
			const opt = document.createElement("option");
			opt.value = device.deviceId;
			opt.textContent = device.label;
			select.appendChild(opt);
		}
		const selected = opts.getSelected();
		if (selected !== undefined) select.value = selected;
	};
	render();

	parent.event(toggle, "click", () => {
		visible = !visible;
		render();
	});

	parent.event(select, "change", () => {
		opts.onSelected(select.value);
		visible = false;
		render();
	});

	return wrapper;
}

import type { Catalog } from "@moq/hang";
import type { Effect } from "@moq/signals";
import * as DOM from "@moq/signals/dom";
import type MoqWatch from "../../element";
import { check, icon } from "../icons";

// A readable label for a caption rendition: prefer its label, then a language + role, then the name.
function label(name: string, config: Catalog.TextConfig): string {
	if (config.label) return config.label;
	if (config.lang) return config.role === "caption" ? `${config.lang} (CC)` : config.lang;
	return name;
}

function detail(config: Catalog.TextConfig): string {
	const parts: string[] = [config.role === "caption" ? "captions" : "subtitles"];
	if (config.format && config.format !== "vtt") parts.push(config.format);
	return parts.join(" · ");
}

function captionRow(
	parent: Effect,
	title: string,
	subtitle: string,
	selected: boolean,
	onSelect: () => void,
): HTMLElement {
	const row = DOM.create("button", { className: "q-row", type: "button" });
	if (selected) row.classList.add("q-row--selected");

	const mark = DOM.create("span", { className: "q-row-check" });
	if (selected) mark.appendChild(icon(check));

	const body = DOM.create("div", { className: "q-row-body" });
	body.append(
		DOM.create("span", { className: "q-row-title" }, title),
		DOM.create("span", { className: "q-row-sub" }, subtitle),
	);

	row.append(mark, body);
	parent.event(row, "click", onSelect);
	return row;
}

/** The Captions tab: turn captions off or pick a text rendition (usually one per language). */
export function captionsTab(parent: Effect, watch: MoqWatch): HTMLElement {
	const container = DOM.create("div", { className: "tab-body q-list" });

	parent.run((effect) => {
		const available = effect.get(watch.text.out.available);
		const selected = effect.get(watch.controls.captions);
		const entries = Object.entries(available);

		container.replaceChildren();

		// Off row: always present so captions can be dismissed.
		container.appendChild(
			captionRow(effect, "Off", "no captions", selected === undefined, () => {
				watch.controls.captions.set(undefined);
			}),
		);

		if (entries.length === 0) {
			container.appendChild(DOM.create("div", { className: "tab-empty" }, "No caption tracks"));
			return;
		}

		for (const [name, config] of entries) {
			container.appendChild(
				captionRow(effect, label(name, config), detail(config), selected === name, () => {
					watch.controls.captions.set(name);
				}),
			);
		}
	});

	return container;
}

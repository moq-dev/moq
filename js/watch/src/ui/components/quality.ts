import type { Catalog } from "@moq/hang";
import type { Effect } from "@moq/signals";
import * as DOM from "@moq/signals/dom";
import type MoqWatch from "../../element";
import { formatBitrate } from "../format";
import { check, icon } from "../icons";

function resolution(config: Catalog.VideoConfig): string | undefined {
	if (config.codedWidth && config.codedHeight) return `${config.codedWidth}×${config.codedHeight}`;
	return undefined;
}

function detail(config: Catalog.VideoConfig): string {
	const parts: string[] = [];
	if (config.framerate) parts.push(`${Math.round(config.framerate)}fps`);
	if (config.bitrate) parts.push(formatBitrate(config.bitrate));
	if (config.codec) parts.push(config.codec);
	return parts.join(" · ");
}

function qualityRow(
	parent: Effect,
	title: string,
	subtitle: string,
	selected: boolean,
	playing: boolean,
	onSelect: () => void,
): HTMLElement {
	const row = DOM.create("button", { className: "q-row", type: "button" });
	if (selected) row.classList.add("q-row--selected");

	const mark = DOM.create("span", { className: "q-row-check" });
	if (selected) mark.appendChild(icon(check));

	const body = DOM.create("div", { className: "q-row-body" });
	const titleEl = DOM.create("span", { className: "q-row-title" }, title);
	if (playing) {
		const badge = DOM.create("span", { className: "q-row-playing" }, "playing");
		titleEl.appendChild(badge);
	}
	const subEl = DOM.create("span", { className: "q-row-sub" }, subtitle);
	body.append(titleEl, subEl);

	row.append(mark, body);
	parent.event(row, "click", onSelect);
	return row;
}

/** The Quality tab: pick a video rendition or let ABR choose automatically. */
export function qualityTab(parent: Effect, watch: MoqWatch): HTMLElement {
	const container = DOM.create("div", { className: "tab-body q-list" });

	parent.run((effect) => {
		const source = watch.backend.video.source;
		const catalog = effect.get(source.catalog);
		const target = effect.get(source.target);
		const active = effect.get(source.track);
		const renditions = catalog?.renditions ?? {};
		const entries = Object.entries(renditions);

		container.replaceChildren();

		if (entries.length === 0) {
			container.appendChild(DOM.create("div", { className: "tab-empty" }, "No video renditions"));
			return;
		}

		const manual = target?.name;

		// Auto row: ABR picks the best rendition for the available bandwidth.
		const autoSub = active && !manual ? `auto · currently ${active}` : "adapts to bandwidth";
		container.appendChild(
			qualityRow(effect, "Auto", autoSub, !manual, false, () => {
				source.target.update((prev) => ({ ...prev, name: undefined }));
			}),
		);

		// Largest first so the list reads top-down high to low.
		entries.sort((a, b) => {
			const sa = (a[1].codedWidth ?? 0) * (a[1].codedHeight ?? 0);
			const sb = (b[1].codedWidth ?? 0) * (b[1].codedHeight ?? 0);
			return sb - sa;
		});

		for (const [name, config] of entries) {
			const title = resolution(config) ?? name;
			container.appendChild(
				qualityRow(effect, title, detail(config), manual === name, active === name, () => {
					source.target.update((prev) => ({ ...prev, name }));
				}),
			);
		}
	});

	return container;
}

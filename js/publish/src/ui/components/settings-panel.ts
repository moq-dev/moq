import type { Effect } from "@moq/signals";
import * as DOM from "@moq/signals/dom";
import type MoqPublish from "../../element";
import { camera, close, icon, settings as settingsIcon, video as videoIcon } from "../icons";
import type { Tab, UiState } from "../state";
import { sourceTab } from "./source-tab";
import { statsTab } from "./stats-tab";

const TABS: { id: Tab; label: string; svg: string }[] = [
	{ id: "source", label: "Source", svg: camera },
	{ id: "stats", label: "Stats", svg: videoIcon },
];

/** The slide-in settings sheet: tab strip + the active tab's content. */
export function settingsPanel(parent: Effect, publish: MoqPublish, state: UiState): HTMLElement {
	const panel = DOM.create("div", { className: "panel" });
	panel.setAttribute("role", "dialog");
	panel.setAttribute("aria-label", "Publish settings");

	const header = DOM.create("div", { className: "panel-head" });
	const titleWrap = DOM.create("div", { className: "panel-head-title" });
	titleWrap.append(icon(settingsIcon), DOM.create("span", {}, "Settings"));
	const closeBtn = DOM.create("button", { className: "control flex-center", type: "button" });
	closeBtn.title = "Close";
	closeBtn.setAttribute("aria-label", "Close settings");
	closeBtn.appendChild(icon(close));
	parent.event(closeBtn, "click", () => state.panel.set(false));
	header.append(titleWrap, closeBtn);

	// Tab strip (ARIA tablist). IDs are scoped to this shadow root.
	const PANEL_ID = "panel-body";
	const strip = DOM.create("div", { className: "panel-tabs" });
	strip.setAttribute("role", "tablist");
	const tabButtons = TABS.map((tab) => {
		const btn = DOM.create("button", { className: "panel-tab", type: "button", id: `tab-${tab.id}` });
		btn.setAttribute("role", "tab");
		btn.setAttribute("aria-controls", PANEL_ID);
		btn.append(icon(tab.svg), DOM.create("span", {}, tab.label));
		parent.event(btn, "click", () => state.tab.set(tab.id));
		strip.appendChild(btn);
		return { tab, btn };
	});

	const body = DOM.create("div", { className: "panel-body", id: PANEL_ID });
	body.setAttribute("role", "tabpanel");

	parent.run((effect) => {
		const active = effect.get(state.tab);
		body.setAttribute("aria-labelledby", `tab-${active}`);
		for (const { tab, btn } of tabButtons) {
			const selected = tab.id === active;
			btn.classList.toggle("panel-tab--active", selected);
			btn.setAttribute("aria-selected", String(selected));
			btn.tabIndex = selected ? 0 : -1;
		}
	});

	parent.run((effect) => {
		if (!effect.get(state.panel)) return;
		const tab = effect.get(state.tab);
		const content = tab === "source" ? sourceTab(effect, publish) : statsTab(effect, publish);
		DOM.render(effect, body, content);
	});

	panel.append(header, strip, body);

	parent.run((effect) => {
		const open = effect.get(state.panel);
		panel.classList.toggle("panel--open", open);
		panel.setAttribute("aria-hidden", String(!open));
	});

	return panel;
}

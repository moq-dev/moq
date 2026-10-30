import * as DOM from "@moq/signals/dom";
import { icon } from "./button";

/** Which accent a {@link statCard} takes. */
export type StatKind = "network" | "video" | "audio";

/** A stats card created by {@link statCard}. */
export interface StatCard {
	/** The root element to mount. */
	el: HTMLElement;
	/** The key/value grid that {@link statLine} appends rows to. */
	grid: HTMLElement;
	/** A status pill in the header, hidden until the caller shows it. */
	status: HTMLElement;
}

/** A stats card with an icon, title, and status pill over a key/value grid. */
export function statCard(kind: StatKind, label: string, svg: string): StatCard {
	const el = DOM.create("div", { className: `stat-card stat-card--${kind}` });

	const head = DOM.create("div", { className: "stat-head" });
	const iconWrap = DOM.create("div", { className: "stat-icon" });
	iconWrap.appendChild(icon(svg));
	const status = DOM.create("span", { className: "stat-status", style: { display: "none" } });
	head.append(iconWrap, DOM.create("span", { className: "stat-title" }, label), status);

	const grid = DOM.create("div", { className: "stat-grid" });
	el.append(head, grid);
	return { el, grid, status };
}

/** Append a labeled row to a card's grid, returning the value span to fill in. */
export function statLine(grid: HTMLElement, label: string): HTMLSpanElement {
	const row = DOM.create("div", { className: "stat-line" });
	const value = DOM.create("span", { className: "stat-value" }, "—");
	row.append(DOM.create("span", { className: "stat-key" }, label), value);
	grid.appendChild(row);
	return value;
}

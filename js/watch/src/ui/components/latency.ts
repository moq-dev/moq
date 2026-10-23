import { Milli } from "@moq/clock";
import type { Effect } from "@moq/signals";
import * as DOM from "@moq/signals/dom";
import type MoqWatch from "../../element";
import { formatMillis } from "../format";
import { bufferControl } from "./buffer-control";

type Preset = { label: string; value: "auto" | number };

const PRESETS: Preset[] = [
	{ label: "Auto", value: "auto" },
	{ label: "100ms", value: 100 },
	{ label: "250ms", value: 250 },
	{ label: "500ms", value: 500 },
	{ label: "1s", value: 1000 },
	{ label: "2s", value: 2000 },
];

/** The Latency tab: choose a playback delay and watch the live buffer timeline. */
export function latencyTab(parent: Effect, watch: MoqWatch): HTMLElement {
	const container = DOM.create("div", { className: "tab-body latency" });

	// Preset chips.
	const chips = DOM.create("div", { className: "latency-presets" });
	const buttons = PRESETS.map((preset) => {
		const chip = DOM.create("button", { className: "chip", type: "button" }, preset.label);
		parent.event(chip, "click", () => {
			watch.delay = preset.value === "auto" ? "auto" : Milli(preset.value);
		});
		chips.appendChild(chip);
		return { preset, chip };
	});

	parent.run((effect) => {
		const mode = effect.get(watch.controls.delay);
		for (const { preset, chip } of buttons) {
			const active = preset.value === "auto" ? mode === "auto" : mode === preset.value;
			chip.classList.toggle("chip--active", active);
		}
	});

	// The draggable buffered-range timeline.
	const timeline = bufferControl(parent, watch);

	// Numeric readout: the resolved delay.
	const readout = DOM.create("div", { className: "latency-readout" });
	const delayStat = DOM.create("div", { className: "latency-stat" });
	const delayVal = DOM.create("span", { className: "latency-stat-value" }, "—");
	delayStat.append(DOM.create("span", { className: "latency-stat-label" }, "Delay"), delayVal);
	readout.append(delayStat);

	parent.run((effect) => {
		const mode = effect.get(watch.controls.delay);
		const delay = effect.get(watch.sync.out.delay);
		delayVal.textContent = `${formatMillis(delay)}${mode === "auto" ? " (auto)" : ""}`;
	});

	const hint = DOM.create(
		"div",
		{ className: "tab-hint" },
		"A larger delay smooths over network jitter at the cost of latency. Auto follows how late frames actually arrive. Drag the timeline to fine-tune.",
	);

	container.append(chips, timeline, readout, hint);
	return container;
}

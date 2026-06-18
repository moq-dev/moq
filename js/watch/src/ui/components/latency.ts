import type { Moq } from "@moq/hang";
import type { Effect } from "@moq/signals";
import * as DOM from "@moq/signals/dom";
import type MoqWatch from "../../element";
import { latencyBounds } from "../../sync";
import { formatMillis } from "../format";
import { bufferControl } from "./buffer-control";

type Preset = { label: string; value: "real-time" | number };

const PRESETS: Preset[] = [
	{ label: "Real-time", value: "real-time" },
	{ label: "100ms", value: 100 },
	{ label: "250ms", value: 250 },
	{ label: "500ms", value: 500 },
	{ label: "1s", value: 1000 },
	{ label: "2s", value: 2000 },
];

/** The Latency tab: choose a buffer target and watch the live buffer timeline. */
export function latencyTab(parent: Effect, watch: MoqWatch): HTMLElement {
	const container = DOM.create("div", { className: "tab-body latency" });

	// Preset chips.
	const chips = DOM.create("div", { className: "latency-presets" });
	const buttons = PRESETS.map((preset) => {
		const chip = DOM.create("button", { className: "chip", type: "button" }, preset.label);
		parent.event(chip, "click", () => {
			watch.latencyMin = preset.value === "real-time" ? "real-time" : (preset.value as Moq.Time.Milli);
		});
		chips.appendChild(chip);
		return { preset, chip };
	});

	parent.run((effect) => {
		const mode = latencyBounds(effect.get(watch.backend.latency)).min;
		for (const { preset, chip } of buttons) {
			const active = preset.value === "real-time" ? mode === "real-time" : mode === preset.value;
			chip.classList.toggle("chip--active", active);
		}
	});

	// The draggable buffered-range timeline.
	const timeline = bufferControl(parent, watch);

	// Numeric readout: resolved jitter + total end-to-end buffer.
	const readout = DOM.create("div", { className: "latency-readout" });
	const jitterStat = DOM.create("div", { className: "latency-stat" });
	const jitterVal = DOM.create("span", { className: "latency-stat-value" }, "—");
	jitterStat.append(DOM.create("span", { className: "latency-stat-label" }, "Jitter buffer"), jitterVal);
	const bufferStat = DOM.create("div", { className: "latency-stat" });
	const bufferVal = DOM.create("span", { className: "latency-stat-value" }, "—");
	bufferStat.append(DOM.create("span", { className: "latency-stat-label" }, "Total buffer"), bufferVal);
	readout.append(jitterStat, bufferStat);

	parent.run((effect) => {
		const mode = latencyBounds(effect.get(watch.backend.latency)).min;
		const jitter = effect.get(watch.backend.jitter);
		const total = effect.get(watch.backend.sync.buffer);
		jitterVal.textContent = `${formatMillis(jitter)}${mode === "real-time" ? " (auto)" : ""}`;
		bufferVal.textContent = formatMillis(total);
	});

	const hint = DOM.create(
		"div",
		{ className: "tab-hint" },
		"A larger buffer smooths over network jitter at the cost of latency. Real-time tracks the connection RTT automatically. Drag the timeline to fine-tune.",
	);

	container.append(chips, timeline, readout, hint);
	return container;
}

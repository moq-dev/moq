import type { Effect } from "@moq/signals";
import * as DOM from "@moq/signals/dom";
import type MoqWatch from "../../element";
import type { UiState } from "../state";
import { fullscreenButton } from "./fullscreen-button";
import { liveBadge } from "./live-badge";
import { playPauseButton } from "./play-pause";
import { settingsButton } from "./settings-button";
import { volumeSlider } from "./volume-slider";

/** The bottom control bar: essentials on the left, settings + fullscreen on the right. */
export function controlBar(parent: Effect, watch: MoqWatch, state: UiState, player: HTMLElement): HTMLElement {
	const bar = DOM.create("div", { className: "controls" });

	const left = DOM.create("div", { className: "controls-group" });
	left.append(playPauseButton(parent, watch), volumeSlider(parent, watch), liveBadge(parent, watch, state));

	const spacer = DOM.create("div", { className: "controls-spacer" });

	const right = DOM.create("div", { className: "controls-group" });
	right.append(settingsButton(parent, state), fullscreenButton(parent, player, watch));

	bar.append(left, spacer, right);
	return bar;
}

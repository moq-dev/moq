import type { Effect } from "@moq/signals";
import * as DOM from "@moq/signals/dom";
import type MoqPublish from "../../element";
import type { UiState } from "../state";
import { audioToggle } from "./audio-toggle";
import { fullscreenButton } from "./fullscreen-button";
import { settingsButton } from "./settings-button";
import { statusBadge } from "./status-badge";
import { videoToggle } from "./video-toggle";

/** The bottom control bar: capture toggles + status on the left, settings + fullscreen on the right. */
export function controlBar(parent: Effect, publish: MoqPublish, state: UiState, player: HTMLElement): HTMLElement {
	const bar = DOM.create("div", { className: "controls" });

	const left = DOM.create("div", { className: "controls-group" });
	left.append(audioToggle(parent, publish), videoToggle(parent, publish), statusBadge(parent, publish));

	const spacer = DOM.create("div", { className: "controls-spacer" });

	const right = DOM.create("div", { className: "controls-group" });
	right.append(settingsButton(parent, state), fullscreenButton(parent, player));

	bar.append(left, spacer, right);
	return bar;
}

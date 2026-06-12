import type { Signal } from "@moq/signals";

export type Tab = "quality" | "latency" | "stats";

/** Shared reactive UI chrome state, threaded through the control components. */
export interface UiState {
	// Whether the overlay chrome (top + bottom bars) is currently shown.
	chrome: Signal<boolean>;
	// Whether the settings sheet is open.
	panel: Signal<boolean>;
	// The active settings tab.
	tab: Signal<Tab>;
}

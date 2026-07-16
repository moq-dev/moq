// Cross-browser fullscreen for the player.
//
// We fullscreen the shadow `.player` container (not the bare <moq-watch>) so the
// overlay chrome stays visible. Safari needs webkit-prefixed methods, and iPhone
// has no element fullscreen API at all: there we fall back to a CSS pseudo-fullscreen
// that pins the player to the viewport. The native iOS fullscreen API is only exposed
// on <video>, which we never render into.
import type { Effect } from "@moq/signals";

type WebkitDocument = Document & {
	webkitFullscreenElement?: Element | null;
	webkitExitFullscreen?: () => void;
};

type WebkitElement = HTMLElement & {
	webkitRequestFullscreen?: () => void;
};

const PSEUDO_CLASS = "player--pseudo-fullscreen";

export interface Fullscreen {
	/** Whether the player is currently maximized (real or pseudo fullscreen). */
	active(): boolean;
	/** Enter if windowed, exit if fullscreen. Must run inside a user gesture. */
	toggle(): void;
	/** Subscribe to state changes; returns an unsubscribe function. */
	onChange(fn: () => void): () => void;
}

/**
 * @param parent Effect that owns the document listeners (auto-removed on cleanup).
 * @param player The shadow container to maximize.
 */
export function createFullscreen(parent: Effect, player: HTMLElement): Fullscreen {
	const doc = document as WebkitDocument;
	const listeners = new Set<() => void>();
	const notify = () => {
		for (const fn of listeners) fn();
	};

	// Real fullscreen changes (incl. Esc / browser chrome) flow through notify too.
	parent.event(document, "fullscreenchange", notify);
	parent.event(document, "webkitfullscreenchange", notify);

	const realActive = () => !!(document.fullscreenElement || doc.webkitFullscreenElement);
	const pseudoActive = () => player.classList.contains(PSEUDO_CLASS);
	const active = () => realActive() || pseudoActive();

	const enter = () => {
		const el = player as WebkitElement;
		if (el.requestFullscreen) {
			// Promise may reject (denied / not a user gesture): fall back gracefully.
			el.requestFullscreen().catch(() => enterPseudo());
			return;
		}
		if (el.webkitRequestFullscreen) {
			el.webkitRequestFullscreen();
			return;
		}

		// iPhone: no element fullscreen API, so pin the player to the viewport instead.
		enterPseudo();
	};

	const exit = () => {
		if (pseudoActive()) {
			exitPseudo();
			return;
		}
		if (document.exitFullscreen) {
			document.exitFullscreen().catch(() => {});
			return;
		}
		doc.webkitExitFullscreen?.();
	};

	const enterPseudo = () => {
		player.classList.add(PSEUDO_CLASS);
		notify();
	};
	const exitPseudo = () => {
		player.classList.remove(PSEUDO_CLASS);
		notify();
	};

	const toggle = () => (active() ? exit() : enter());

	const onChange = (fn: () => void): (() => void) => {
		listeners.add(fn);
		return () => listeners.delete(fn);
	};

	return { active, toggle, onChange };
}

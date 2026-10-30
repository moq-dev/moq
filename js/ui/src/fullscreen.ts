// Cross-browser fullscreen for the watch player and the publish preview.
//
// We fullscreen the shadow `.player` container (not the bare custom element) so the
// overlay chrome stays visible. Safari needs webkit-prefixed methods, and iPhone
// has no element fullscreen API at all: there we fall back to a CSS pseudo-fullscreen
// that pins the player to the viewport. The native iOS fullscreen API is only exposed
// on <video>, which we never render into.
import { type Effect, type Getter, Signal } from "@moq/signals";

type WebkitDocument = Document & {
	webkitFullscreenElement?: Element | null;
	webkitExitFullscreen?: () => void;
};

type WebkitElement = HTMLElement & {
	webkitRequestFullscreen?: () => void;
};

const PSEUDO_CLASS = "player--pseudo-fullscreen";

/** A fullscreen controller created by {@link createFullscreen}. */
export interface Fullscreen {
	/** Whether the player is currently maximized (real or pseudo fullscreen). */
	active: Getter<boolean>;
	/** Enter if windowed, exit if fullscreen. Must run inside a user gesture. */
	toggle(): void;
}

/**
 * Maximize `player`, falling back to the `player--pseudo-fullscreen` class where the browser
 * has no element fullscreen API or refuses the request. The overlay styles that class.
 *
 * @param parent Effect that owns the document listeners (auto-removed on cleanup).
 * @param player The shadow container to maximize.
 */
export function createFullscreen(parent: Effect, player: HTMLElement): Fullscreen {
	const doc = document as WebkitDocument;

	const realActive = () => !!(document.fullscreenElement || doc.webkitFullscreenElement);
	const pseudoActive = () => player.classList.contains(PSEUDO_CLASS);

	const active = new Signal(realActive() || pseudoActive());
	const refresh = () => active.set(realActive() || pseudoActive());

	// Real fullscreen changes (incl. Esc / browser chrome) refresh the state too.
	parent.event(document, "fullscreenchange", refresh);
	parent.event(document, "webkitfullscreenchange", refresh);

	const enterPseudo = () => {
		player.classList.add(PSEUDO_CLASS);
		refresh();
	};
	const exitPseudo = () => {
		player.classList.remove(PSEUDO_CLASS);
		refresh();
	};

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

	// Read the live state rather than the signal, whose writes land a microtask later.
	const toggle = () => (realActive() || pseudoActive() ? exit() : enter());

	return { active, toggle };
}

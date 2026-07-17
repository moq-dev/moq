import { type Effect, Signal } from "@moq/signals";

/**
 * Resume a suspended {@link AudioContext} from a real user gesture.
 *
 * The context is built at page load, before any user activation can exist, so browsers that
 * gate audio on a gesture reject a `resume()` made then. A single unconditional attempt would
 * fire once, be rejected, and never retry, leaving audio silent. This instead attempts
 * `resume()` immediately (for autoplay-permissive browsers like Chrome with prior engagement),
 * then retries on every `pointerdown`/`keydown` until the context is actually running, dropping
 * the gesture listeners once it is. `pointerdown` and `keydown` cover mouse, touch, pen, and
 * keyboard, and each carries a user activation.
 *
 * Safari also reports an "interrupted" state (a WebKit-only value outside the
 * suspended/running/closed set) and can leave it on its own; mirroring `statechange` into a
 * signal picks that up so the listeners are re-armed or dropped as the state moves.
 *
 * Scoped to `effect`: the listeners are removed when the effect reruns or closes.
 */
export function unlockOnGesture(effect: Effect, context: AudioContext): void {
	const running = new Signal(context.state === "running");
	effect.event(context, "statechange", () => running.set(context.state === "running"));

	effect.run((inner) => {
		if (inner.get(running)) return;

		const resume = () => {
			context.resume().catch(() => {});
		};

		resume();
		inner.event(document, "pointerdown", resume);
		inner.event(document, "keydown", resume);
	});
}

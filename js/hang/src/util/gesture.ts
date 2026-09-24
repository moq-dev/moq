import { type Effect, type Getter, Signal } from "@moq/signals";

/**
 * Resume a suspended {@link AudioContext} from a real user gesture, returning whether it is running.
 *
 * A context built before any user activation starts suspended in browsers that gate audio on a
 * gesture, and a `resume()` made then is rejected. A single unconditional attempt would fire once,
 * be rejected, and never retry, leaving the graph silent. This instead attempts `resume()`
 * immediately (for autoplay-permissive browsers like Chrome with prior engagement), then retries on
 * every gesture until the context is actually running, dropping the listeners once it is. A mouse
 * grants activation on `pointerdown` but touch and pen only on `pointerup`, so both are listened to,
 * plus `keydown`.
 *
 * Safari also reports an "interrupted" state (a WebKit-only value outside the
 * suspended/running/closed set) and can leave it on its own; mirroring `statechange` into the
 * returned signal picks that up so the listeners are re-armed or dropped as the state moves.
 *
 * Scoped to `effect`: the listeners are removed when the effect reruns or closes.
 */
export function unlock(effect: Effect, context: AudioContext): Getter<boolean> {
	const running = new Signal(context.state === "running");
	effect.event(context, "statechange", () => running.set(context.state === "running"));

	effect.run((inner) => {
		if (inner.get(running)) return;

		const resume = () => {
			context.resume().catch(() => {});
		};

		resume();
		inner.event(document, "pointerdown", resume);
		inner.event(document, "pointerup", resume);
		inner.event(document, "keydown", resume);
	});

	return running;
}

import type { Bandwidth } from "@moq/net";
import type { Effect, Getter } from "@moq/signals";

/**
 * Hold one viewer on `viewers` while `max` is nonzero, keeping its ceiling at `max`.
 *
 * The claim is released when `max` drops to zero or `undefined`, `viewers` changes or goes away, or
 * `effect` closes. A ceiling the registry refuses, such as one from a catalog whose
 * bitrates sum past what PROBE encodes, is skipped with a warning rather than
 * stopping playback, as is a negative or non-finite one.
 *
 * @internal
 */
export function holdViewer(
	effect: Effect,
	viewers: Getter<Pick<Bandwidth.Viewers, "watch"> | undefined>,
	max: Getter<number | undefined>,
): void {
	const held = effect.computed((inner) => (inner.get(max) ?? 0) !== 0);
	effect.run((outer) => {
		const registry = outer.get(viewers);
		if (!registry || !outer.get(held)) return;

		let viewer: Bandwidth.Viewer;
		try {
			viewer = registry.watch(max.peek() ?? 0);
		} catch (err) {
			console.warn("[Player] viewer ceiling refused; not claiming a viewer target", err);
			// Try again once the ceiling changes.
			outer.get(max);
			return;
		}
		outer.cleanup(() => viewer.close());

		outer.run((inner) => {
			const ceiling = inner.get(max) ?? 0;
			// Zero closes the claim through `held` instead.
			if (ceiling === 0) return;
			try {
				viewer.update(ceiling);
			} catch (err) {
				console.warn("[Player] viewer ceiling refused; keeping the previous one", err);
			}
		});
	});
}

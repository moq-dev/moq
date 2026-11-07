import type { Bandwidth } from "@moq/net";
import { type Effect, type Getter, Signal } from "@moq/signals";

/**
 * Hold one viewer on `viewers` while `max` is nonzero, keeping its ceiling at `max`, and return its
 * grant: the viewer's slice of the receive estimate, or `undefined` without a claim or an estimate.
 *
 * The claim is released when `max` drops to zero or `undefined`, `viewers` changes or goes away, or
 * `effect` closes. A ceiling the registry refuses, such as a negative one or one from a catalog
 * whose bitrates sum past what PROBE encodes, releases the claim with a warning rather than
 * stopping playback, until `max` changes again.
 *
 * @internal
 */
export function holdViewer(
	effect: Effect,
	viewers: Getter<Pick<Bandwidth.Viewers, "watch"> | undefined>,
	max: Getter<number | undefined>,
): Getter<number | undefined> {
	const grant = new Signal<number | undefined>(undefined);

	effect.run((outer) => {
		const registry = outer.get(viewers);
		if (!registry) return;

		let viewer: Bandwidth.Viewer | undefined;
		outer.cleanup(() => {
			viewer?.close();
			grant.set(undefined);
		});

		outer.run((inner) => {
			const ceiling = inner.get(max) ?? 0;
			if (ceiling !== 0) {
				try {
					if (viewer) viewer.update(ceiling);
					else viewer = registry.watch(ceiling);
					const held = viewer;
					inner.run((nested) => grant.set(nested.get(held.rate)));
					return;
				} catch (err) {
					console.warn("[Player] viewer ceiling refused; releasing the claim until it changes", err);
				}
			}
			viewer?.close();
			viewer = undefined;
			grant.set(undefined);
		});
	});

	return grant;
}

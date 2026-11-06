import type { Bandwidth } from "@moq/net";
import type { Effect, Getter } from "@moq/signals";

/**
 * Hold one viewer on `viewers` while `max` is nonzero, keeping its ceiling at `max`.
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
): void {
	effect.run((outer) => {
		const registry = outer.get(viewers);
		if (!registry) return;

		let viewer: Bandwidth.Viewer | undefined;
		outer.cleanup(() => viewer?.close());

		outer.run((inner) => {
			const ceiling = inner.get(max) ?? 0;
			if (ceiling !== 0) {
				try {
					if (viewer) viewer.update(ceiling);
					else viewer = registry.watch(ceiling);
					return;
				} catch (err) {
					console.warn("[Player] viewer ceiling refused; releasing the claim until it changes", err);
				}
			}
			viewer?.close();
			viewer = undefined;
		});
	});
}

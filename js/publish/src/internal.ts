import type * as Moq from "@moq/net";
import type { Effect, Getter } from "@moq/signals";

/**
 * @internal Whether a subscriber is attached to `track`, scoped to `effect`. Changes only when that
 * does, not with every group the subscribers want, so a bandwidth reservation keyed on it holds.
 */
export function watched(track: Moq.Track.Producer, effect: Effect): Getter<boolean | undefined> {
	return effect.computed((demand) => demand.get(track.demand().wanted) !== undefined);
}

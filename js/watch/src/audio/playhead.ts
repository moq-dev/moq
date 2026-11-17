import type * as Container from "@moq/hang/container";
import { Derived } from "@moq/signals";
import type { Sync } from "../sync";
import { bound } from "./latency";

/**
 * The oldest media the ring still plays, which the audio consumer reads as its clock: the shared
 * clock less the ring's {@link bound}. The ring stretches audio trailing the clock within the bound
 * back onto it, so a missing group can still play until the group past it trails the clock by more
 * than that. A late burst often fills a hole a moment after the group past it lands.
 *
 * @internal Exported for the replay harness.
 */
export function playable(sync: Sync): Container.Playhead {
	const trail = new Derived([sync.out.delay, sync.out.maxDelay, sync.in.stretch], (delay, maxDelay, stretch) =>
		bound({ delay, maxDelay, stretch }),
	);
	return {
		now: () => sync.now()?.saturatingSub(trail.peek()),
		anchor: new Derived([sync.anchor, trail], (anchor, trail) => ({ anchor, trail })),
	};
}

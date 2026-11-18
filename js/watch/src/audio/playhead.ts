import { Duration } from "@moq/clock";
import type * as Container from "@moq/hang/container";
import { Derived, type Getter, Signal } from "@moq/signals";
import type { Sync } from "../sync";
import { WINDOW } from "./buffer";
import { bound } from "./latency";

/**
 * The oldest media the ring still plays, which the audio consumer reads as its clock. The ring renders
 * `output` (none until known) ahead of the shared clock, so what it renders now sounds when the clock
 * reaches it, and stretches audio trailing that within its {@link bound} back onto it, at least one stretch window. So a missing
 * group can still play until the group past it trails the ring by more than the bound, as when a late
 * burst fills the hole a moment after the group past it lands.
 *
 * @internal Exported for the replay harness.
 */
export function playable(sync: Sync, output: Getter<Duration | undefined> = new Signal(undefined)): Container.Playhead {
	// How far the oldest playable media trails the clock, in ms; negative when the output leads by more.
	const trail = new Derived(
		[sync.out.delay, sync.out.maxDelay, sync.in.stretch, output],
		(delay, maxDelay, stretch, output) =>
			// The ring skips only past its bound, never less than one stretch window, even with the stretch off.
			Math.max(bound({ delay, maxDelay, stretch }).asMillis(), WINDOW / 1000) - (output?.asMillis() ?? 0),
	);
	return {
		now: () => {
			// The signed position, since before media zero's turn the ring may already render it.
			const position = sync.position();
			if (position === undefined) return undefined;
			return Duration.millis(Math.max(0, position - trail.peek()));
		},
		anchor: new Derived([sync.anchor, trail], (anchor, trail) => ({ anchor, trail })),
	};
}

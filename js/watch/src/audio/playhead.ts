import { Duration } from "@moq/clock";
import type * as Container from "@moq/hang/container";
import { Derived, type Getter, Signal } from "@moq/signals";
import type { Sync } from "../sync";
import { bound } from "./latency";

/**
 * The oldest media the ring still plays, which the audio consumer reads as its clock. The ring renders
 * `output` (none until known) ahead of the shared clock, so the sample it renders now sounds when the clock reaches it,
 * and stretches audio trailing that within its {@link bound} back onto it. So a missing group can
 * still play until the group past it trails the ring by more than the bound, as when a late burst
 * fills the hole a moment after the group past it lands.
 *
 * @internal Exported for the replay harness.
 */
export function playable(sync: Sync, output: Getter<Duration | undefined> = new Signal(undefined)): Container.Playhead {
	const trail = new Derived(
		[sync.out.delay, sync.out.maxDelay, sync.in.stretch, output],
		(delay, maxDelay, stretch, output) =>
			bound({ delay, maxDelay, stretch }).asMicros() - (output?.asMicros() ?? 0),
	);
	return {
		now: () => {
			const now = sync.now();
			if (now === undefined) return undefined;
			return Duration.micros(Math.max(0, now.asMicros() - trail.peek()));
		},
		anchor: new Derived([sync.anchor, trail], (anchor, trail) => ({ anchor, trail })),
	};
}

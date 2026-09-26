import type * as Catalog from "@moq/hang/catalog";
import { Time } from "@moq/net";

// How far the incoming rendition may still trail the picture it replaces when we promote it,
// absorbing scheduling noise so the switch doesn't hinge on landing inside a single frame
// interval. This is also the largest step backwards a switch can make visible.
const SLACK = Time.Milli(100);

/**
 * How far behind live a rendition's playhead can sit while still being at its own live edge.
 *
 * Its catalog `delay` behind the broadcast's earliest rendition, plus its own spread: frames arrive
 * one group at a time, so the sync buffer has to cover the group cadence or playback starves
 * between groups. The catalog `jitter` wins. Otherwise assume the publisher flushes each frame as
 * it's encoded, so a frame interval is the longest we wait. Undefined when the catalog declares
 * none of them.
 */
export function renditionJitter(config: Catalog.VideoConfig): Time.Milli | undefined {
	let spread: Time.Milli | undefined;
	if (config.jitter !== undefined) spread = Time.Milli(config.jitter);
	else if (config.framerate) spread = Time.Milli(Math.ceil(1000 / config.framerate));

	if (config.delay === undefined) return spread;
	return Time.Milli.add(Time.Milli(config.delay), spread ?? Time.Milli.zero);
}

/** The playheads involved in promoting a new rendition. */
export interface CaughtUp {
	/** The incoming rendition's playhead: the most recent timestamp it has rendered. */
	playhead: Time.Milli;

	/** The outgoing rendition's playhead, or undefined when it has rendered nothing. */
	active?: Time.Milli;
}

/**
 * Whether the incoming rendition has caught up enough to take over the picture.
 *
 * The bar is the outgoing playhead rather than the live edge, which is a moving target: the sync
 * buffer grows the moment a coarser rendition is selected, dropping live below the outgoing
 * playhead, and it runs ahead of both playheads whenever delivery is late, because the sync
 * reference only ever moves down. Barring on live would promote in the first case (replaying
 * everything between the two playheads) and stall the switch outright in the second. Waiting on
 * the outgoing playhead always terminates: live recovers at wall-clock rate, and the incoming
 * rendition fills its buffer meanwhile.
 */
export function caughtUp(props: CaughtUp): boolean {
	// Nothing is rendering from the outgoing rendition, so there's no picture to step back from.
	if (props.active === undefined) return true;

	return Time.Milli.add(props.playhead, SLACK) >= props.active;
}

/** The rendition delays that can be present during a track handoff. */
export interface SwitchJitter {
	/** The delay required by the rendition currently on screen. */
	active?: Time.Milli;

	/** The delay required by the rendition preparing to replace it. */
	pending?: Time.Milli;
}

/** The delay Sync must cover while a rendition switch is in flight. */
export function switchJitter(props: SwitchJitter): Time.Milli | undefined {
	if (props.active === undefined) return props.pending;
	if (props.pending === undefined) return props.active;
	return Time.Milli.max(props.active, props.pending);
}

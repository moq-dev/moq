import type * as Catalog from "@moq/hang/catalog";
import { Time } from "@moq/net";

// How far the incoming rendition may still trail when we promote it, absorbing scheduling noise
// so the switch doesn't hinge on landing inside a single frame interval. This is also the largest
// step backwards the picture can take on a switch.
const SLACK = Time.Milli(100);

/**
 * How far behind live a rendition's playhead can sit while still being at its own live edge.
 *
 * Frames arrive one group at a time, so this is the rendition's group cadence: the sync buffer
 * has to cover it or playback starves between groups. The catalog value wins. Otherwise assume
 * the publisher flushes each frame as it's encoded, so a frame interval is the longest we wait.
 * Undefined when the catalog declares neither.
 */
export function renditionJitter(config: Catalog.VideoConfig): Time.Milli | undefined {
	if (config.jitter !== undefined) return Time.Milli(config.jitter);
	if (config.framerate) return Time.Milli(Math.ceil(1000 / config.framerate));
	return undefined;
}

/** The playheads involved in promoting a new rendition. */
export interface CaughtUp {
	/** The incoming rendition's playhead: the most recent timestamp it has rendered. */
	playhead: Time.Milli;

	/** The outgoing rendition's playhead, or undefined when it has rendered nothing. */
	active?: Time.Milli;

	/** Where playback should be right now, or undefined before the sync clock has an anchor. */
	live?: Time.Milli;
}

/**
 * Whether the incoming rendition has caught up enough to take over the picture.
 *
 * The bar is whichever is lower: where playback should be right now, or where the outgoing
 * rendition actually is. Neither works alone. Live alone stalls the switch whenever delivery
 * runs late, because the sync reference only ever moves down: both playheads sit behind live and
 * neither can reach it. The outgoing playhead alone stalls it once the buffer grows for a coarser
 * rendition, because playheads never rewind, so it sits ahead of live until wall-clock time makes
 * up the difference, freezing the picture for that whole interval.
 */
export function caughtUp(props: CaughtUp): boolean {
	// Nothing is rendering from the outgoing rendition, so there's no picture to step back from.
	if (props.active === undefined) return true;

	const bar = props.live !== undefined ? Time.Milli.min(props.active, props.live) : props.active;
	return Time.Milli.add(props.playhead, SLACK) >= bar;
}

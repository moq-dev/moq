import type * as Catalog from "@moq/hang/catalog";
import { Time } from "@moq/net";

// Extra slack on top of a rendition's own jitter, absorbing scheduling noise so a switch
// doesn't hinge on landing inside a single frame interval.
const SLACK = Time.Milli(100);

/**
 * How far behind live a rendition's playhead can sit while still being at its own live edge.
 *
 * The catalog value wins. Otherwise assume the publisher flushes each frame as it's encoded,
 * so a frame interval is the longest we wait between frames. Undefined when it declares neither.
 */
export function renditionJitter(config: Catalog.VideoConfig): Time.Milli | undefined {
	if (config.jitter !== undefined) return Time.Milli(config.jitter);
	if (config.framerate) return Time.Milli(Math.ceil(1000 / config.framerate));
	return undefined;
}

/** A rendition's playhead measured against the live playhead. */
export interface CaughtUp {
	/** The most recent timestamp the rendition has rendered. */
	playhead: Time.Milli;

	/** The timestamp playback should be at right now, from the shared sync clock. */
	live: Time.Milli;

	/** The rendition's jitter, per {@link renditionJitter}. */
	jitter: Time.Milli;
}

/**
 * Whether a rendition's playhead has caught up to live.
 *
 * Frames arrive one group at a time, so a rendition trails live by up to its own jitter even
 * when nothing is wrong: a 2s segmented rendition can never hold the playhead of one flushing
 * every frame. A flat threshold therefore either stalls a coarse rendition's switch forever or
 * fires only in the instant after a group lands.
 */
export function caughtUp(props: CaughtUp): boolean {
	return Time.Milli.sub(props.live, props.playhead) <= Time.Milli.add(props.jitter, SLACK);
}

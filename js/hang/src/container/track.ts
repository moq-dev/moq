/**
 * Track properties for the media tracks a hang broadcast publishes.
 *
 * The TypeScript twin of `hang::container::track_info`; keep the two in step, since a browser
 * publisher and a Rust one feed the same relays and the same segmented egress.
 *
 * @module
 */

import type { Milli } from "@moq/clock";
import { Time, Track } from "@moq/net";

/**
 * Track properties for a track carrying media frames, for `Request.accept`.
 *
 * Pins the timescale to microseconds (the timestamps hang's containers stamp), and keeps the
 * authored {@link Track.DEFAULT_MAX_AGE}: superseded groups stay fresh long enough for a
 * FETCH-based consumer to reach back over a full playlist window. A segmented egress (HLS/DASH)
 * may only advertise segments a fetch can still reach, and a standard player starts several
 * target durations behind live.
 *
 * Options override that max age; see {@link TrackInfoOptions.maxAge}.
 */
export function trackInfo(options: TrackInfoOptions): Track.InfoInit {
	return {
		timescale: Time.Timescale.MICRO,
		maxAge: options.maxAge === undefined ? Track.DEFAULT_MAX_AGE : options.maxAge,
		priority: options.priority,
	};
}

/** Options for {@link trackInfo}. */
export type TrackInfoOptions = {
	/**
	 * The publisher's tie-break priority, which should come from `PRIORITY` in `@moq/hang/catalog`
	 * for the kind of media the track carries (`PRIORITY.video` for a video track, and so on).
	 *
	 * Required rather than defaulted, matching `hang::container::track_info`: there is no correct
	 * value for "some media track", and leaving it at zero is what puts audio and video in one
	 * undifferentiated tier for every relay deciding what to forward first.
	 */
	priority: number;

	/**
	 * How long, in milliseconds, a superseded group stays fresh, so a fetch or a subscriber's
	 * `delay.max` can still reach it; see `Track.Info.maxAge`. Omitted or `undefined` is
	 * {@link Track.DEFAULT_MAX_AGE}, and `null` sets no publisher limit.
	 *
	 * A freshness bound, not a delivery budget, so it never makes anyone play further behind live
	 * and lowering it does not reduce latency: it only shortens how far back a fetch can reach.
	 * It keeps no bytes either; the cache decides how long a group's payload stays held.
	 */
	maxAge?: Milli | null;
};

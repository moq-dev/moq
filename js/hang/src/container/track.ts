/**
 * Track properties for the media tracks a hang broadcast publishes.
 *
 * The TypeScript twin of `hang::container::{LATENCY_MAX, track_info}`; keep the two in step,
 * since a browser publisher and a Rust one feed the same relays and the same segmented egress.
 *
 * @module
 */
import { Time, type Track } from "@moq/net";

/**
 * How long a media track asks its publisher (and, through TRACK_INFO, every relay) to keep a
 * non-latest group fetchable, in milliseconds.
 *
 * Media is the one thing on a broadcast read as HISTORY rather than followed at the live edge: a
 * segmented egress (HLS/DASH) may only advertise segments a FETCH can still reach, and a standard
 * player starts several target durations behind live. moq-net's default is sized for a live-edge
 * follower, and leaves such a player addressing groups that are already gone.
 *
 * This is a RETENTION budget, not a delivery one, so it does not make anyone play further behind
 * live: it caps what a subscriber may ask to wait for, and a subscriber's own budget still defaults
 * to zero (skip the moment a newer group arrives).
 *
 * Must match `hang::container::LATENCY_MAX` in rs/hang/src/container/frame.rs.
 */
export const LATENCY_MAX_MS = 30_000;

/**
 * Track properties for a track carrying media frames, for `Request.accept`.
 *
 * Pins the timescale to microseconds (the timestamps hang's containers stamp) and declares
 * {@link LATENCY_MAX_MS} so a FETCH-based consumer can reach back over a full playlist window.
 * Every media track a publisher accepts should use this rather than the bare defaults, which are
 * sized for the catalog and timeline: both are read at the live edge, which is retained
 * unconditionally, so neither pays for history it never serves.
 */
export function trackInfo(latencyMax: number = LATENCY_MAX_MS): Partial<Track.Info> {
	return { timescale: Time.Timescale.MICRO, latencyMax };
}

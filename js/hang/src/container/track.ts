/**
 * Track properties for the media tracks a hang broadcast publishes.
 *
 * The TypeScript twin of `hang::container::track_info`; keep the two in step, since a browser
 * publisher and a Rust one feed the same relays and the same segmented egress.
 *
 * @module
 */
import { Time, type Track } from "@moq/net";

// How long a media track asks its publisher (and, through TRACK_INFO, every relay) to keep a
// non-latest group fetchable. Must match `hang::container::LATENCY_MAX` in
// rs/hang/src/container/frame.rs.
const LATENCY_MAX_MS = 30_000;

/**
 * Track properties for a track carrying media frames, for `Request.accept`.
 *
 * Pins the timescale to microseconds (the timestamps hang's containers stamp), and declares a
 * retention long enough for a FETCH-based consumer to reach back over a full playlist window: a
 * segmented egress (HLS/DASH) may only advertise segments a fetch can still reach, and a standard
 * player starts several target durations behind live. Every media track a publisher accepts should
 * use this rather than the bare defaults, which are sized for the catalog and timeline: both are
 * read at the live edge, which is retained unconditionally, so neither pays for history it never
 * serves.
 *
 * `latencyMax` (milliseconds) overrides that retention. It is a RETENTION budget, not a delivery
 * one, so it never makes anyone play further behind live and lowering it does not reduce latency:
 * it only shortens how far back a fetch can reach.
 */
export function trackInfo(latencyMax: number = LATENCY_MAX_MS): Partial<Track.Info> {
	return { timescale: Time.Timescale.MICRO, latencyMax };
}

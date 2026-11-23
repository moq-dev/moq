import { Duration } from "@moq/clock";
import type { Track } from "@moq/net";

/**
 * A live join reaching back past every group, so it takes each group still within `max` of the live
 * edge, as a subscription-wide max age did before moq-lite-07. Test groups often start at their
 * sequence, not their time, so a join placed by time alone would land on the newest. Mirrors the
 * Rust moq-mux `test_util::within`. @internal
 */
export function within(max: Duration | null): Track.Subscription {
	// The largest lag every implementation carries losslessly.
	return { ranges: [{ start: { live: { min: Duration.millis(Number.MAX_SAFE_INTEGER), max } } }] };
}

import { Duration } from "@moq/clock";
import type { Track } from "@moq/net";

/**
 * A live join with no lag limit, so it takes each group still within `max` of the live edge, as a
 * subscription-wide max age did before moq-lite-07. Test groups often start at their sequence, not
 * their time, so a join placed by time alone would land on the newest. Mirrors the Rust moq-mux
 * `test_util::within`. @internal
 */
export function within(max: Duration | null): Track.Subscription {
	return { ranges: [{ start: { live: { min: null, max } } }] };
}

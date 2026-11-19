/**
 * The page's media clock: the timeline every `@moq/publish` timestamp is stamped on.
 *
 * It counts microseconds from instant zero, whose wall time the catalog advertises as `sync.clock`,
 * and runs at the wall clock's rate rather than `performance.now()`'s, so `wall + pts` stays on UTC
 * for any session length. A monotonic clock drifts off UTC by its oscillator's error (seconds a day
 * on macOS and Windows), so each read nudges the timeline toward `Date.now()` by at most 500 ppm,
 * NTP's own slew cap: a wall clock that steps is followed that slowly, and media never jumps back.
 *
 * A browser has no clock that counts sleep, so a forward wall jump of more than two seconds between
 * reads is taken as a suspend: no media exists for that time, so the timeline jumps the gap. A
 * forward NTP step that large is rare in a browser, and a gap only moves forward.
 *
 * Mirrors `moq_mux::Clock`, which is per broadcast; the page has one, since its mapping is the page's.
 * A clock installed with another origin starts a new timeline, such as a test's `Manual`, whose wall
 * time never drifts from its instants, so it stamps them as they are.
 *
 * @module
 */

import { Duration, type Instant } from "@moq/clock";
import { read } from "./media-timeline";

/** The current timestamp on the media clock, since instant zero. */
export function now(): Duration {
	return Duration.micros(read().reading);
}

/**
 * The instant `at` on the media clock: as far before or after {@link now} as `at` is before or
 * after the current instant. Use it for a capture time or a deadline read off the installed clock.
 */
export function capture(at: Instant): Duration {
	const { at: present, reading } = read();
	const micros = at.ge(present) ? reading + at.since(present).asMicros() : reading - present.since(at).asMicros();
	return Duration.micros(Math.max(micros, 0));
}

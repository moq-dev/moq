import { Duration } from "@moq/clock";
import type { Live, Range, Subscription } from "./track.ts";

/** A range as a test states it under one subscription-wide max: a live join may give its lag alone. @internal */
export type LegacyRange = { start?: bigint | { live: Duration | Live }; end?: bigint };

/** A subscription as a test states it under one subscription-wide `delay.max`, `null` for none. @internal */
export type LegacySubscription = Omit<Subscription, "ranges"> & {
	delay?: { max?: Duration | null };
	ranges?: readonly LegacyRange[];
};

/**
 * `subscription` under its `delay.max` the way one subscription-wide max applied it: the oldest
 * group, and the default ranges, take every group still within it (or every group for `null`); a
 * live join keeps its lag and takes it; a time start is history either way. Mirrors the Rust test
 * helper `Subscription::with_max`. @internal
 */
export function withMax(subscription: LegacySubscription): Subscription {
	const { delay, ranges, ...rest } = subscription;
	const max = delay?.max === undefined ? Duration.ZERO : delay.max;
	const within = (): Range["start"] =>
		max === null ? undefined : { live: { min: null, max } };
	const out = (ranges ?? [{}]).map((range): Range => {
		const start = range.start;
		const mapped: Range = {};
		if (start === undefined) {
			const joined = within();
			if (joined !== undefined) mapped.start = joined;
		} else if (typeof start === "bigint") mapped.start = start;
		else mapped.start = { live: { min: start.live instanceof Duration ? start.live : start.live.min, max } };
		if (range.end !== undefined) mapped.end = range.end;
		return mapped;
	});
	return { ...rest, ranges: out };
}

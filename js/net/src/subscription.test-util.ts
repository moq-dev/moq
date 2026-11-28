import { Duration } from "@moq/clock";
import { type Live, type Order, type Range, Subscription } from "./track.ts";
import { type Policy, policyOf, rangesPolicy, withPolicy } from "./window.ts";

/** A range as a test states it under one subscription-wide max: a live join may give its lag alone. @internal */
export type LegacyRange = {
	start?: bigint | { live: Duration | Live };
	end?: bigint;
	priority?: number;
	order?: Order;
};

/**
 * A subscription as a test states it under one subscription-wide `delay.max`, `null` for none, and
 * one priority and order every range takes. @internal
 */
export type LegacySubscription = Omit<Subscription, "ranges"> & {
	delay?: { max?: Duration | null };
	priority?: number;
	order?: Order;
	ranges?: readonly LegacyRange[];
};

/** The priority and order `subscription`'s ranges ask as a whole, or nothing without one. @internal */
export function topPolicy(subscription: Subscription | undefined): Policy | undefined {
	return subscription && rangesPolicy(subscription.ranges ?? []);
}

/** `subscription` with every range at `priority` and `order`, as one subscription-wide policy asked. @internal */
export function everyRange(subscription: Subscription, policy: { priority?: number; order?: Order }): Subscription {
	// The defaults left out, as the model holds them.
	const ranges = (subscription.ranges ?? Subscription.default().ranges ?? []).map((range) =>
		withPolicy(range, {
			priority: policy.priority ?? policyOf(range).priority,
			order: policy.order ?? policyOf(range).order,
		}),
	);
	return { ...subscription, ranges };
}

/**
 * `subscription` under its `delay.max` the way one subscription-wide max applied it: the oldest
 * group, and the default ranges, take every group still within it (or every group for `null`); a
 * live join keeps its lag and takes it; a time start is history either way. Mirrors the Rust test
 * helper `Subscription::with_max`. @internal
 */
export function withMax(subscription: LegacySubscription): Subscription {
	const { delay, ranges, priority, order, ...rest } = subscription;
	const max = delay?.max === undefined ? Duration.ZERO : delay.max;
	const within = (): Range["start"] => (max === null ? undefined : { live: { min: null, max } });
	const out = (ranges ?? [{}]).map((range): Range => {
		const start = range.start;
		const mapped: Range = {};
		if (start === undefined) {
			const joined = within();
			if (joined !== undefined) mapped.start = joined;
		} else if (typeof start === "bigint") mapped.start = start;
		else mapped.start = { live: { min: start.live instanceof Duration ? start.live : start.live.min, max } };
		if (range.end !== undefined) mapped.end = range.end;
		if (range.priority !== undefined) mapped.priority = range.priority;
		if (range.order !== undefined) mapped.order = range.order;
		return mapped;
	});
	return everyRange({ ...rest, ranges: out }, { priority, order });
}

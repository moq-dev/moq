import { Time } from "@moq/net";
import type { Signal } from "@moq/signals";

/** One received frame, including a latency skip immediately before it. */
export interface Arrival {
	/** Media presentation time in milliseconds. */
	timestamp: Time.Milli;
	/** Monotonic transport ingress time in milliseconds. */
	arrival: Time.Milli;
	/** Whether ingress missed the playback deadline. */
	late: boolean;
	/** Whether latency enforcement skipped content before this frame. */
	skipped: boolean;
}

/** Retain up to 30 seconds and 4096 frames without losing coalesced signal updates. @internal */
export function recordArrival(window: Signal<readonly Arrival[]>, frame: Arrival): void {
	window.update((previous) => {
		let newest = frame.arrival;
		for (const entry of previous) newest = Time.Milli.max(newest, entry.arrival);
		const recent = previous.filter((entry) => entry.arrival >= newest - 30_000);
		if (frame.arrival >= newest - 30_000) recent.push(frame);
		return recent.slice(-4096);
	});
}

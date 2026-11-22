import { Duration, sleep } from "@moq/clock";

/** Waits between rechecks, 75 s in all: npm's CDN lags a fresh publish by tens of seconds. */
export const DELAYS: Duration[] = [5, 10, 15, 20, 25].map((s) => Duration.secs(s));

/**
 * Recheck each spec that is missing from the registry after each delay, and return those still missing once the delays run out.
 * `found` must throw on a registry failure, which is never retried.
 */
export async function settle(
	missing: string[],
	found: (spec: string) => boolean,
	delays: Duration[] = DELAYS,
): Promise<string[]> {
	let pending = missing;
	for (const delay of delays) {
		if (pending.length === 0) break;
		console.log(`⏳ Waiting ${delay.asSecs()}s for npm to serve ${pending.join(", ")}...`);
		await sleep(delay);
		pending = pending.filter((spec) => !found(spec));
	}
	return pending;
}

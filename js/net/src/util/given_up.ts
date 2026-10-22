import { race } from "@moq/signals";
import type { Consumer as GroupConsumer } from "../group.ts";

// Why to stop writing `group`: the subscription's budget gave it up, or it failed for good
// before it was read whole (evicted, or its copy aborted with no route to continue it).
function reason(group: GroupConsumer): Error | undefined {
	const closed = group.closed.peek();
	return group.expired.peek() ?? (closed instanceof Error ? closed : undefined);
}

/**
 * Run a publisher's write of `group`, settling with why to stop it instead if that comes
 * first: `expired`, or `closed` with an error. A clean close is no reason, and a group
 * already given up never starts the write. Rejects if the write does. Mirrors the Rust
 * publishers' `poll_given_up`.
 */
export async function untilGivenUp(group: GroupConsumer, write: () => Promise<unknown>): Promise<Error | undefined> {
	let stop = reason(group);
	if (stop) return stop;
	const done = write().then(() => true as const);
	for (;;) {
		const closed = group.closed.peek() === undefined ? [group.closed] : [];
		if ((await race([done, group.expired, ...closed])) === true) return undefined;
		stop = reason(group);
		if (stop) return stop;
	}
}

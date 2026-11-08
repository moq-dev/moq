import type { Backend } from "./backend.ts";

// The bun test preload sets `test` while each test runs; `real` is its hostTime() opt-out.
export const guard: { test?: { real: boolean } } = {};

// The origin is the realm's performance time origin, so DOM timestamps (rAF, events, media
// callbacks) convert to an instant by unit alone.
export const host: Backend = {
	now: () => performance.now() * 1_000_000,
	wall: () => Date.now(),
	origin: () => performance.timeOrigin,
	arm(at, ready, ref) {
		// A timer fails only the test that armed it, and only while that test runs: one firing
		// after its test ended didn't make the test wait.
		const owner = guard.test;
		// Read at arm time, since the failure's own stack only names the host callback.
		const armed = owner && !owner.real ? new Error("armed").stack : undefined;
		let timer: ReturnType<typeof setTimeout> | undefined;
		let active = true;
		const check = () => {
			if (!active) return;
			const left = at - host.now();
			if (left <= 0) {
				active = false;
				ready();
				// Thrown from the host callback, so the test running now fails.
				if (armed !== undefined && owner === guard.test && !owner?.real && timer !== undefined) {
					throw new Error(
						`a timer fired on the host clock: install a Manual clock or await the event, or call hostTime()\n${armed}`,
					);
				}
			} else {
				// Hosts clamp oversized delays to 1ms and may round shorter ones down.
				timer = setTimeout(check, Math.min(Math.ceil(left / 1_000_000), 2_147_483_647));
				if (!ref) (timer as unknown as { unref?: () => void }).unref?.();
			}
		};
		check();
		return () => {
			active = false;
			if (timer !== undefined) clearTimeout(timer);
		};
	},
	task(ready) {
		// Message ports schedule a task without the nested timer's minimum delay.
		const channel = new MessageChannel();
		channel.port1.onmessage = () => {
			channel.port1.close();
			channel.port2.close();
			ready();
		};
		channel.port2.postMessage(null);
	},
};

// A host task drains every already-queued microtask, including promise chains;
// counting Promise.resolve() turns would impose an arbitrary depth limit.
export function drain(): Promise<void> {
	return new Promise((resolve) => host.task(resolve));
}

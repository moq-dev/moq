import type { Backend } from "./backend.ts";

// The origin is the realm's performance time origin, so DOM timestamps (rAF, events, media
// callbacks) convert to an instant by unit alone.
export const host: Backend = {
	now: () => performance.now() * 1_000_000,
	wall: () => Date.now(),
	origin: () => performance.timeOrigin,
	arm(at, ready, ref) {
		let timer: ReturnType<typeof setTimeout> | undefined;
		let active = true;
		const check = () => {
			if (!active) return;
			const left = at - host.now();
			if (left <= 0) {
				active = false;
				ready();
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

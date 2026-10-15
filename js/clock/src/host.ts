import type { Backend } from "./backend.ts";

let origin: number | undefined;

export const host: Backend = {
	now: () => {
		const reading = performance.now();
		origin ??= reading;
		return (reading - origin) * 1_000_000;
	},
	wall: () => Date.now(),
	arm(at, ready) {
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
			}
		};
		check();
		return () => {
			active = false;
			if (timer !== undefined) clearTimeout(timer);
		};
	},
};

// A host task drains every already-queued microtask, including promise chains;
// counting Promise.resolve() turns would impose an arbitrary depth limit.
export function drain(): Promise<void> {
	return new Promise((resolve) => {
		const channel = new MessageChannel();
		channel.port1.onmessage = () => {
			channel.port1.close();
			channel.port2.close();
			resolve();
		};
		channel.port2.postMessage(null);
	});
}

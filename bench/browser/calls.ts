/**
 * Init scripts for the sampling scenario, installed before any page module runs.
 *
 * Each function is serialized into the page, so it must not close over anything.
 */

/**
 * Counts `WebTransport.getStats` calls and every `setTimeout`, the primitive behind
 * `@moq/clock` sleeps, on `globalThis.__sampling`. One increment per call, in every pass and
 * every variant. The page patches `@moq/qmux`'s `getStats` into the same counter.
 */
export function install(): void {
	const counts = { getstats: 0, timers: 0 };
	Object.defineProperty(globalThis, "__sampling", { value: counts });
	const timeout = globalThis.setTimeout;
	globalThis.setTimeout = ((handler: TimerHandler, delay?: number, ...args: unknown[]) => {
		counts.timers++;
		return timeout(handler, delay, ...args);
	}) as typeof setTimeout;
	const transport = (globalThis as { WebTransport?: { prototype: { getStats?: () => Promise<unknown> } } })
		.WebTransport?.prototype;
	const getStats = transport?.getStats;
	if (transport && getStats)
		transport.getStats = function (this: unknown) {
			counts.getstats++;
			return getStats.call(this);
		};
}

/** Removes WebTransport, as in a browser lacking it, so every dial takes the qmux fallback. */
export function withoutWebTransport(): void {
	Object.defineProperty(globalThis, "WebTransport", { value: undefined, configurable: true, writable: true });
}

/** Deletes WebTransport getStats(), which shipping Chromium and Safari do not expose. */
export function withoutGetStats(): void {
	const transport = (globalThis as { WebTransport?: { prototype: { getStats?: unknown } } }).WebTransport;
	if (transport) delete transport.prototype.getStats;
}

import { type Nano, sleep } from "@moq/clock";

/**
 * Run `fn` after `duration` on the installed clock, and return the call that cancels it.
 *
 * `ref: false` lets a Node or Bun process exit while only this timer is pending.
 *
 * @internal
 */
export function after(duration: Nano, fn: () => void, options?: { ref?: boolean }): () => void {
	const abort = new AbortController();
	void sleep(duration, { signal: abort.signal, ref: options?.ref }).then(() => {
		// The wakeup can already be queued when the timer is cancelled.
		if (!abort.signal.aborted) fn();
	}, ignore);
	return () => abort.abort();
}

function ignore(): void {}

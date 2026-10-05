import { race } from "@moq/signals";

// Settle with `promise`, or reject with the signal's reason once it aborts. An already-aborted
// signal rejects at once. There's no cancellation of `promise` itself; the caller releases
// whatever it holds when this rejects.
export async function untilAborted<T>(promise: Promise<T>, signal?: AbortSignal): Promise<T> {
	if (!signal) return promise;
	signal.throwIfAborted();

	const { promise: aborted, reject } = Promise.withResolvers<never>();
	const onAbort = () => reject(signal.reason);
	signal.addEventListener("abort", onAbort, { once: true });
	try {
		return await race([promise, aborted]);
	} finally {
		signal.removeEventListener("abort", onAbort);
	}
}

import { Elapsed, type Nano, timeout } from "@moq/clock";

export class TimeoutError extends Error {
	constructor(message: string) {
		super(message);
		this.name = "TimeoutError";
	}
}

// Race `promise` against a timeout on the installed clock. There's no cancellation; if
// `promise` settles after the timeout fires the caller is responsible for cleaning up any
// side effects (open streams, etc.) it may produce.
export async function withTimeout<T>(promise: Promise<T>, duration: Nano, message: string): Promise<T> {
	try {
		return await timeout(duration, promise);
	} catch (err) {
		if (err instanceof Elapsed) throw new TimeoutError(message);
		throw err;
	}
}

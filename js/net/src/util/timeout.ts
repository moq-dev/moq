export class TimeoutError extends Error {
	constructor(message: string) {
		super(message);
		this.name = "TimeoutError";
	}
}

// Race `promise` against a timeout. There's no cancellation; if `promise`
// settles after the timeout fires the caller is responsible for cleaning up
// any side effects (open streams, etc.) it may produce.
export function withTimeout<T>(promise: Promise<T>, ms: number, message: string): Promise<T> {
	let timer: ReturnType<typeof setTimeout>;
	const timeout = new Promise<never>((_, reject) => {
		timer = setTimeout(() => reject(new TimeoutError(message)), ms);
	});
	return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

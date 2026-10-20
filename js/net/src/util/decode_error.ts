// Preserve the original typed error: data decoders use its type to recover locally.
const malformed = new WeakSet<Error>();

export function decodeError(cause: unknown): Error {
	const err = cause instanceof Error ? cause : new Error(String(cause));
	malformed.add(err);
	return err;
}

export function isDecodeError(cause: unknown): boolean {
	return cause instanceof Error && malformed.has(cause);
}

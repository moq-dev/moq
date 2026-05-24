// I hate javascript.
export function error(err: unknown): Error {
	return err instanceof Error ? err : new Error(String(err));
}

export function unreachable(value: never): never {
	throw new Error(`unreachable: ${value}`);
}

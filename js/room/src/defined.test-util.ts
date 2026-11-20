/** `value`, failing the test when it is `undefined`, as a name or group already taken. @internal */
export function defined<T>(value: T | undefined): T {
	if (value === undefined) throw new Error("expected a value");
	return value;
}

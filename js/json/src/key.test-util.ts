// Scale-less writes for tests: a scale-less track's writes name their group keys.
import { Time, type Timed } from "@moq/net";

let counter = 0n;

/** Restart {@link key} at 0, so each test's first group is 0: run it before each test. */
export function resetKeys(): void {
	counter = 0n;
}

/** The next key of a scale-less track: keys only have to increase, so one counter serves every track a test writes. */
export function key(): Time.Timestamp {
	return Time.Timestamp.unscaled(counter++);
}

/** `value` at the next key; see {@link key}. */
export function next<T>(value: T): Timed<T> {
	return { value, at: key() };
}

/** `value` at scale-less `key`. */
export function keyed<T>(value: T, key: bigint): Timed<T> {
	return { value, at: Time.Timestamp.unscaled(key) };
}

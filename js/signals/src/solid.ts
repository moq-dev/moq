import {
	createSignal,
	onCleanup,
	type Accessor as SolidAccessor,
	type Setter as SolidSetter,
	type Signal as SolidSignal,
} from "solid-js";
import type { Getter, Setter, Signal } from "./index";

// A helper to create a Solid accessor from a signal.
export function createAccessor<T>(signal: Getter<T>): SolidAccessor<T> {
	// Disable the equals check because we do it ourselves.
	const [get, set] = createSignal(signal.peek(), { equals: false });
	const dispose = signal.subscribe((value) => set(() => value));
	onCleanup(() => dispose());
	return get;
}

// A helper to create a Solid [get, set] pair from a signal.
export function createPair<T>(signal: Signal<T>): SolidSignal<T> {
	const [get, set] = createSignal(signal.peek(), { equals: false });

	// Sync from our signal to Solid
	const dispose = signal.subscribe((value) => set(() => value));
	onCleanup(() => dispose());

	// Sync from Solid to our signal
	const originalSet = set as (...args: unknown[]) => unknown;
	const wrappedSet = ((...args: unknown[]) => {
		const result = originalSet(...args);
		signal.set(get());
		return result;
	}) as typeof set;

	return [get, wrappedSet];
}

// A helper to create a Solid setter from a signal.
export function createSetter<T>(signal: Setter<T>): SolidSetter<T> {
	return ((value: T | ((prev: T) => T)) => {
		if (typeof value === "function") {
			signal.update(value as (prev: T) => T);
		} else {
			signal.set(value);
		}
		return value;
	}) as SolidSetter<T>;
}

/** @deprecated Use `createAccessor` instead. */
const solid = createAccessor;
export default solid;

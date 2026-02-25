import {
	createSignal,
	onCleanup,
	type Accessor as SolidAccessor,
	type Setter as SolidSetter,
	type Signal as SolidSignal,
} from "solid-js";
import type { Getter, Signal } from "./index";

// A helper to create a Solid accessor from a signal.
export function createAccessor<T>(signal: Getter<T>): SolidAccessor<T> {
	// Disable the equals check because we do it ourselves.
	const [get, set] = createSignal(signal.peek(), { equals: false });
	const dispose = signal.subscribe((value) => set(() => value));
	onCleanup(() => dispose());
	return get;
}

// A helper to create a Solid setter that writes to a signal.
export function createSetter<T>(signal: Signal<T>): SolidSetter<T> {
	const setter = (value: T | ((prev: T) => T)) => {
		if (typeof value === "function") {
			signal.update(value as (prev: T) => T);
		} else {
			signal.set(value);
		}
		return signal.peek();
	};
	return setter as SolidSetter<T>;
}

// A helper to create a Solid [get, set] pair from a signal.
export function createPair<T>(signal: Signal<T>): SolidSignal<T> {
	return [createAccessor(signal), createSetter(signal)];
}

/** @deprecated Use `createAccessor` instead. */
const solid = createAccessor;
export default solid;

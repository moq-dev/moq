/**
 * SolidJS adapters that bridge signals to Solid accessors and setters.
 *
 * @module
 */

import {
	createSignal,
	onCleanup,
	type Accessor as SolidAccessor,
	type Setter as SolidSetter,
	type Signal as SolidSignal,
} from "solid-js";
import type { Getter, Signal } from "./index";

/** Creates a Solid accessor that tracks the given signal, unsubscribing on cleanup. */
export function createAccessor<T>(signal: Getter<T>): SolidAccessor<T> {
	// Disable the equals check because we do it ourselves.
	const [get, set] = createSignal(signal.peek(), { equals: false });
	const dispose = signal.subscribe((value) => set(() => value));
	onCleanup(() => dispose());
	return get;
}

/** Creates a Solid setter that writes through to the given signal. */
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

/** Creates a Solid `[accessor, setter]` pair backed by the given signal. */
export function createPair<T>(signal: Signal<T>): SolidSignal<T> {
	return [createAccessor(signal), createSetter(signal)];
}

/** @deprecated Use `createAccessor` instead. */
const solid = createAccessor;
export default solid;

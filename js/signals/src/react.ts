import { useCallback, useSyncExternalStore } from "react";
import type { Getter, Signal } from "./index";

// A helper to read a signal's value in React.
export function useValue<T>(signal: Getter<T>): T {
	return useSyncExternalStore(
		(callback) => signal.subscribe(callback),
		() => signal.peek(),
		() => signal.peek(),
	);
}

// A helper to read and write a signal in React, like useState.
export function useSignal<T>(signal: Signal<T>): [T, (value: T | ((prev: T) => T)) => void] {
	const value = useValue(signal);
	const setter = useCallback(
		(next: T | ((prev: T) => T)) => {
			if (typeof next === "function") {
				signal.update(next as (prev: T) => T);
			} else {
				signal.set(next);
			}
		},
		[signal],
	);
	return [value, setter];
}

/** @deprecated Use `useValue` instead. */
const react = useValue;
export default react;

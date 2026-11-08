/**
 * React hooks for reading and writing signals from components.
 *
 * @module
 */

import { useCallback, useMemo, useSyncExternalStore } from "react";
import type { Getter, Signal } from "./index";

/** Subscribes a component to a signal and returns its current value. */
export function useValue<T>(signal: Getter<T>): T {
	// React compares snapshots by identity, which misses an in-place `mutate()` and loops on a
	// readable whose `peek()` builds a new object per call. Snapshot a version bumped per
	// delivery instead, and render the delivered value.
	const store = useMemo(() => {
		let value = signal.peek();
		let version = 0;
		return {
			subscribe(callback: () => void) {
				// Catch a write between the render's peek and this subscription.
				const latest = signal.peek();
				if (!Object.is(latest, value)) {
					value = latest;
					version++;
				}
				return signal.subscribe((next) => {
					value = next;
					version++;
					callback();
				});
			},
			version: () => version,
			value: () => value,
		};
	}, [signal]);

	useSyncExternalStore(store.subscribe, store.version, store.version);
	return store.value();
}

/** Reads and writes a signal as a `[value, setValue]` pair, like `useState`. */
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

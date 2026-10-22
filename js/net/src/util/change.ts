import type { Dispose, Getter } from "@moq/signals";

/**
 * Resolve with "changed" on the next change to any of `sources`, listening from now.
 *
 * Unlike the lazy `Signal.race`, this registers before the caller's next await, so a change
 * landing while it works still wakes the wait. Call `dispose` once done waiting.
 */
export function nextChange(...sources: (Getter<unknown> | undefined)[]): {
	changed: Promise<"changed">;
	dispose: Dispose;
} {
	const { promise: changed, resolve } = Promise.withResolvers<"changed">();
	const disposers = sources.map((source) => source?.changed(() => resolve("changed")));
	return {
		changed,
		dispose: () => {
			for (const dispose of disposers) dispose?.();
		},
	};
}

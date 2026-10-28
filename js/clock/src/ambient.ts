import type { Backend } from "./backend.ts";
import { host } from "./host.ts";

let current: { backend: Backend; previous?: typeof current; active: boolean } = { backend: host, active: true };

/** A clock installed in the current realm; close it to restore the clock it replaced. */
export interface Installed {
	/** Restore the clock this one replaced. Idempotent, and safe out of order. */
	close(): void;
}

export function backend(): Backend {
	return current.backend;
}

export function install(backend: Backend): Installed {
	const entry = { backend, previous: current, active: true };
	current = entry;
	return {
		close: () => {
			entry.active = false;
			// Out-of-order cleanup must not revive an already-restored outer clock.
			while (!current.active && current.previous) current = current.previous;
		},
	};
}

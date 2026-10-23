import type { Backend } from "./backend.ts";
import { host } from "./host.ts";

let current: { backend: Backend; previous?: typeof current; active: boolean } = { backend: host, active: true };

/** A clock installed in the current realm; close it to restore the clock it replaced. */
export interface Installed extends Disposable {
	/** Restore the clock this one replaced. Idempotent, and safe out of order. */
	close(): void;
}

export function backend(): Backend {
	return current.backend;
}

export function install(backend: Backend): Installed {
	const entry = { backend, previous: current, active: true };
	current = entry;
	const close = () => {
		entry.active = false;
		// Out-of-order cleanup must not revive an already-restored outer clock.
		while (!current.active && current.previous) current = current.previous;
	};
	return { close, [Symbol.dispose]: close };
}

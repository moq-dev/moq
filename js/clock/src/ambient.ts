import type { Backend } from "./backend.ts";
import { host } from "./host.ts";

let current: { backend: Backend; previous?: typeof current; active: boolean } = { backend: host, active: true };

export function backend(): Backend {
	return current.backend;
}

export function install(backend: Backend): () => void {
	const entry = { backend, previous: current, active: true };
	current = entry;
	return () => {
		entry.active = false;
		// Out-of-order cleanup must not revive an already-restored outer clock.
		while (!current.active && current.previous) current = current.previous;
	};
}

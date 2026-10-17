import type { Dispose, Getter } from "@moq/signals";
import type { Grant } from "./auth.ts";
import type * as Path from "./path.ts";

// private: plumbing
/** Whether our grant still lets us serve one incoming request, built when it is dispatched. @internal */
export class Gate {
	#grant?: Getter<Grant | undefined>;

	constructor(grant?: Getter<Grant | undefined>) {
		this.#grant = grant;
	}

	/** Whether our grant lets us publish `path` right now. No grant yet allows it. */
	allows(path: Path.Valid): boolean {
		const grant = this.#grant?.peek();
		return grant === undefined || grant.publish.matches(path);
	}

	/** Call `onRevoke` once our grant stops covering `path`. Arm it before the request's first check. */
	watch(path: Path.Valid, onRevoke: () => void): { revoked: () => boolean; dispose: Dispose } {
		let revoked = false;
		const dispose =
			this.#grant?.subscribe(() => {
				if (revoked || this.allows(path)) return;
				revoked = true;
				onRevoke();
			}) ?? (() => {});
		return { revoked: () => revoked || !this.allows(path), dispose };
	}
}

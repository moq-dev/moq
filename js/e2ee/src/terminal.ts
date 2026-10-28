import type * as Net from "@moq/net";
import { Failure } from "./failure.ts";

/**
 * Grouped-failure signal shared by one subscriber and the groups it handed out.
 *
 * Failing closes the net subscription with the failure. `@moq/net` closes every group that
 * subscription handed out with the same error, which wakes their parked reads and ends
 * publisher demand while the e2ee handles are still held.
 */
export class Terminal {
	failed?: Failure;
	#subscriber?: Net.Track.Subscriber;

	constructor(subscriber: Net.Track.Subscriber) {
		this.#subscriber = subscriber;
	}

	/** Throw once grouped authentication has ended this subscriber. */
	check(): void {
		if (this.failed) throw this.failed;
	}

	/** End the subscriber: wake every parked read and release publisher demand. */
	fail(): Failure {
		if (this.failed) return this.failed;
		const failed = new Failure("authentication");
		this.failed = failed;
		this.#subscriber?.close(failed);
		this.#subscriber = undefined;
		return failed;
	}

	/** Drop the subscription without failing; held groups end cleanly after what they hold. */
	unsubscribe(): void {
		this.#subscriber?.close();
		this.#subscriber = undefined;
	}
}

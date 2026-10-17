import type * as Net from "@moq/net";
import { Failure } from "./failure.ts";

/**
 * Grouped-failure signal shared by one subscriber and the groups it handed out.
 *
 * A parked read waits on a net object that stays silent after a bad frame, so failing
 * closes each net handle with the failure: that wakes the read, and dropping the handles
 * ends publisher demand while the e2ee handles are still held.
 */
export class Terminal {
	failed?: Failure;
	#subscriber?: Net.Track.Subscriber;
	#groups = new Set<Net.Group.Consumer>();

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
		for (const group of this.#groups) group.close(failed);
		this.#groups.clear();
		return failed;
	}

	/** Drop the subscription without failing, so held groups end cleanly after what they hold. */
	unsubscribe(): void {
		this.#subscriber?.close();
		this.#subscriber = undefined;
	}

	add(group: Net.Group.Consumer): void {
		if (this.failed) group.close(this.failed);
		else this.#groups.add(group);
	}

	delete(group: Net.Group.Consumer): void {
		this.#groups.delete(group);
	}
}

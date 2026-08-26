/**
 * Tracks whether the audio ring still holds samples from a superseded subscription.
 *
 * The ring outlives the subscription that fills it, so reopening one (a rendition swap, a
 * republished broadcast, a reconnect) leaves the previous subscription's decoded audio buffered.
 * The replacement is the authority from its first frame onwards, so everything the ring holds past
 * that timestamp is stale and has to go.
 */
export class Handover {
	#open = false;
	#pending = false;

	/** Record a subscription opening on the current ring. */
	opened(): void {
		if (this.#open) this.#pending = true;
		this.#open = true;
	}

	/** Consume one decoded frame and report whether it is the first of a replacement subscription. */
	takeover(): boolean {
		const pending = this.#pending;
		this.#pending = false;
		return pending;
	}
}

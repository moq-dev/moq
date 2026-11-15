/**
 * Tracks whether the audio ring still holds samples from a superseded subscription.
 *
 * The ring outlives the subscription that fills it, so reopening one on the same broadcast (a
 * rendition swap, a return from an absence) leaves the previous subscription's decoded audio buffered.
 * The replacement is the authority from its first frame onwards, so everything the ring holds past
 * that timestamp is stale and has to go.
 */
export class Handover {
	#open = false;
	#pending = false;
	// An end the replacement declared before its first frame, which its takeover must keep.
	#end?: number;

	/** Record a subscription opening on the current ring. */
	opened(): void {
		if (this.#open) this.#pending = true;
		this.#open = true;
		this.#end = undefined;
	}

	/**
	 * Record an end declared on the ring, which before a replacement's first frame is the replacement's,
	 * or undefined once the next utterance follows it.
	 */
	declared(end: number | undefined): void {
		if (this.#pending) this.#end = end;
	}

	/**
	 * Consume one decoded frame. The first of a replacement subscription takes over, carrying any end
	 * the replacement already declared.
	 */
	takeover(): { end?: number } | undefined {
		if (!this.#pending) return undefined;
		this.#pending = false;
		const end = this.#end;
		this.#end = undefined;
		return { end };
	}
}

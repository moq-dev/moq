/** Which peer-initiated stream indexes have had a frame, so a late STREAM frame
 *  cannot resurrect one whose receive half already reached FIN or RESET_STREAM.
 *
 * Opening stream N implicitly opens every lower one, but those stay unseen until
 * their own first frame. Only the unseen indexes are stored, as ranges: a peer
 * jumping straight to a high index costs one range, and each unseen index holds
 * a stream credit until its first frame, so there are never more ranges than the
 * stream limit we advertised. */
export class RecvOpen {
	// The highest index seen; every index above it is unopened.
	#max = -1n;
	// Unseen indexes at or below #max, as disjoint inclusive ranges in order.
	#holes: [bigint, bigint][] = [];

	/** Whether the stream was seen, which means closed if it isn't live. */
	isClosed(index: bigint): boolean {
		return index <= this.#max && this.#find(index) < 0;
	}

	/** Mark the stream seen, implicitly opening every lower index. */
	record(index: bigint) {
		if (index > this.#max) {
			if (index > this.#max + 1n) this.#holes.push([this.#max + 1n, index - 1n]);
			this.#max = index;
			return;
		}

		const i = this.#find(index);
		if (i < 0) return;
		const [start, end] = this.#holes[i];
		const rest: [bigint, bigint][] = [];
		if (start < index) rest.push([start, index - 1n]);
		if (index < end) rest.push([index + 1n, end]);
		this.#holes.splice(i, 1, ...rest);
	}

	/** How many unseen ranges are tracked. */
	get holes(): number {
		return this.#holes.length;
	}

	#find(index: bigint): number {
		let lo = 0;
		let hi = this.#holes.length - 1;
		while (lo <= hi) {
			const mid = (lo + hi) >> 1;
			const [start, end] = this.#holes[mid];
			if (index < start) hi = mid - 1;
			else if (index > end) lo = mid + 1;
			else return mid;
		}
		return -1;
	}
}

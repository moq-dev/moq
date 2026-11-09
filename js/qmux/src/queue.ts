/** A FIFO queue whose `shift` is amortized O(1), unlike `Array.prototype.shift`,
 *  which is O(n) in V8 and makes draining a peer-sized queue quadratic. */
export class Queue<T> {
	#items: (T | undefined)[] = [];
	#head = 0;

	/** The number of queued items. */
	get length(): number {
		return this.#items.length - this.#head;
	}

	/** Append an item. */
	push(item: T): void {
		this.#items.push(item);
	}

	/** Remove and return the oldest item, if any. */
	shift(): T | undefined {
		if (this.#head === this.#items.length) return undefined;
		const item = this.#items[this.#head];
		this.#items[this.#head++] = undefined;
		// Reclaim the consumed prefix once it is at least half the array, keeping the copy amortized O(1).
		if (this.#head * 2 >= this.#items.length) {
			this.#items = this.#items.slice(this.#head);
			this.#head = 0;
		}
		return item;
	}

	/** Remove every item. */
	clear(): void {
		this.#items = [];
		this.#head = 0;
	}

	/** Iterate the queued items, oldest first, without removing them. */
	*[Symbol.iterator](): Iterator<T> {
		for (let i = this.#head; i < this.#items.length; i++) yield this.#items[i] as T;
	}
}
